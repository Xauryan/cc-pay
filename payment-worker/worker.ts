import { chromium, type Browser, type BrowserContext, type Page, type Route } from "playwright-core";
import { setTimeout as delay } from "node:timers/promises";

interface PaymentInput {
  protocol: 1;
  action: string;
  fields: Record<string, string>;
  amount: string;
  login: string;
  password: string;
  proxy?: string | null;
}
interface AttemptState {
  passwordSubmitted: boolean;
  debitSubmitted: boolean;
  authArmed: boolean;
  commitArmed: boolean;
  rejected?: boolean;
  invalidComponent?: boolean;
  unsupported?: boolean;
}
interface PaymentResult {
  protocol: 1;
  outcome: "uncertain" | "rejected" | "not_submitted";
  password_submitted: boolean;
  debit_submitted: boolean;
  reason: string;
}
function isRecord(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

// Only this process handles the isolated Alipay browser. No browser profiles,
// screenshots, HAR files, cookies, or provider responses are written to disk.

const AUTH_PATH = "/standard/securityPost.json";
const COMMIT_PATH = "/business/api/paycommit.json";
const AUTH_HOST = "excashier.alipay.com";
const PAY_HOSTS = new Set(["cashier.alipay.com", "excashier.alipay.com"]);
const RETURN_HOSTS = new Set(["cashier.cc-pay.cn"]);
const POST_PATHS = new Set([
  "/standard/switchToStdFront.htm",
  "/standard/securityRender.json",
  AUTH_PATH,
  "/business/api/acceptPay.json",
  "/business/api/cashiermain.json",
  "/business/api/switchchannelsel.json",
  "/business/api/error.json",
  COMMIT_PATH,
]);

function money(value: unknown): bigint {
  const match = /^(0|[1-9]\d{0,7})(?:\.(\d{1,2}))?$/.exec(String(value));
  if (!match) throw new Error("invalid_amount");
  return BigInt(match[1]) * 100n + BigInt((match[2] || "").padEnd(2, "0"));
}

function officialHost(host: string): boolean {
  return ["alipay.com", "alipayobjects.com", "alipaylog.com"].some(
    (domain) => host === domain || host.endsWith(`.${domain}`),
  );
}

function allowedUrl(raw: string): boolean {
  try {
    const u = new URL(raw);
    return (
      u.protocol === "https:" &&
      !u.username &&
      !u.password &&
      (!u.port || u.port === "443") &&
      (officialHost(u.hostname) || RETURN_HOSTS.has(u.hostname))
    );
  } catch {
    return false;
  }
}

function validateInput(input: unknown): PaymentInput {
  if (!isRecord(input)) throw new Error("invalid_input");
  if (
    input?.protocol !== 1 ||
    typeof input.login !== "string" ||
    input.login.length < 3 ||
    input.login.length > 254 ||
    /[\r\n]/.test(input.login) ||
    typeof input.password !== "string" ||
    !/^\d{6}$/.test(input.password)
  ) {
    throw new Error("invalid_input");
  }
  if (
    typeof input.action !== "string" ||
    input.action.length > 32768 ||
    !allowedUrl(input.action)
  ) {
    throw new Error("invalid_gateway");
  }
  const action = new URL(input.action);
  if (
    action.hostname !== "openapi.alipay.com" ||
    action.pathname !== "/gateway.do"
  ) {
    throw new Error("invalid_gateway");
  }
  if (
    !isRecord(input.fields) ||
    Object.keys(input.fields).length > 50 ||
    Object.values(input.fields).some(
      (v) => typeof v !== "string" || v.length > 65536,
    )
  ) {
    throw new Error("invalid_form");
  }
  const params = new URLSearchParams(action.search);
  for (const [key, value] of Object.entries(input.fields)) {
    if (params.has(key)) throw new Error("duplicate_field");
    params.set(key, value as string);
  }
  if (!params.get("sign") || params.get("method") !== "alipay.trade.page.pay") {
    throw new Error("unsigned_order");
  }
  const biz: unknown = JSON.parse(params.get("biz_content") || "null");
  if (
    !isRecord(biz) ||
    typeof biz.out_trade_no !== "string" ||
    !biz.out_trade_no.trim() ||
    biz.product_code !== "FAST_INSTANT_TRADE_PAY" ||
    (biz.trans_currency && biz.trans_currency !== "CNY") ||
    money(biz.total_amount) <= 0n ||
    money(biz.total_amount) !== money(input.amount)
  ) {
    throw new Error("order_mismatch");
  }
  if (input.proxy !== null && input.proxy !== undefined) {
    if (typeof input.proxy !== "string") throw new Error("invalid_proxy");
    const proxy = new URL(input.proxy);
    if (
      !["http:", "https:", "socks5:", "socks5h:"].includes(proxy.protocol) ||
      !proxy.hostname ||
      proxy.hash ||
      proxy.search ||
      !["", "/"].includes(proxy.pathname)
    ) {
      throw new Error("invalid_proxy");
    }
  }
  if (typeof input.amount !== "string") throw new Error("invalid_amount");
  return input as unknown as PaymentInput;
}

// Even unauthenticated proxies go through this loopback relay. Chromium sees no
// upstream credentials and has no DIRECT fallback. SOCKS5 supports LAN and auth.
async function startRelay(proxy: string | null | undefined) {
  const { Server } = await import("proxy-chain");
  if (proxy) {
    const parsed = new URL(proxy);
    if (parsed.protocol === "socks5h:") parsed.protocol = "socks5:";
    proxy = parsed.href;
  }
  const relay = new Server({
    host: "127.0.0.1",
    port: 0,
    verbose: false,
    prepareRequestFunction: ({ hostname, port, isHttp }) => {
      if (
        isHttp ||
        port !== 443 ||
        (!officialHost(hostname) && !RETURN_HOSTS.has(hostname))
      ) {
        throw new Error("Destination denied");
      }
      return {
        requestAuthentication: false,
        upstreamProxyUrl: proxy || undefined,
      };
    },
  });
  relay.on("requestFailed", () => {});
  await relay.listen();
  return {
    server: `http://127.0.0.1:${relay.port}`,
    close: () => relay.close(true),
  };
}

function result(state: AttemptState, reason: string): PaymentResult {
  return {
    protocol: 1,
    outcome: state.debitSubmitted
      ? "uncertain"
      : state.rejected
        ? "rejected"
        : "not_submitted",
    password_submitted: state.passwordSubmitted,
    debit_submitted: state.debitSubmitted,
    reason: state.debitSubmitted ? "confirmation_required" : reason,
  };
}

function providerError(value: unknown): boolean {
  // Auth redirects from the recorded official flow carry the refusal code.
  // An HTTP 200 or stat=ok alone is never a successful payment.
  if (!isRecord(value)) return false;
  if (typeof value.errorCode === "string" && value.errorCode.length > 0)
    return true;
  if (Array.isArray(value.errorCode) && value.errorCode.some(Boolean))
    return true;
  try {
    return Boolean(new URL(typeof value.redirectUrl === "string" ? value.redirectUrl : "").searchParams.get("errorCode"));
  } catch {
    return false;
  }
}

function createGate(input: PaymentInput, state: AttemptState) {
  let gatewayUsed = false;
  return async (route: Route) => {
    const request = route.request();
    const raw = request.url();
    if (!allowedUrl(raw)) return route.abort();
    const url = new URL(raw);
    if (raw === input.action && !gatewayUsed && request.isNavigationRequest()) {
      gatewayUsed = true;
      return route.continue({
        method: "POST",
        postData: new URLSearchParams(input.fields).toString(),
        headers: {
          ...request.headers(),
          "content-type": "application/x-www-form-urlencoded",
        },
      });
    }
    if (url.hostname === "openapi.alipay.com") return route.abort();
    if (url.pathname === COMMIT_PATH) {
      if (
        !PAY_HOSTS.has(url.hostname) ||
        request.method() !== "POST" ||
        !state.commitArmed ||
        state.debitSubmitted
      )
        return route.abort();
      state.debitSubmitted = true; // before transport, so timeouts cannot trigger another attempt
    } else if (url.pathname === AUTH_PATH) {
      if (
        url.hostname !== AUTH_HOST ||
        request.method() !== "POST" ||
        !state.authArmed ||
        state.passwordSubmitted
      )
        return route.abort();
      const fields = new URLSearchParams(request.postData() || "");
      const encrypted = fields.get("password") || "";
      if (
        fields.get("loginId") !== input.login ||
        !/^[A-Za-z0-9+/]{342}==$/.test(encrypted) ||
        !fields.get("rdsUa") ||
        !fields.get("securityId")
      ) {
        state.invalidComponent = true;
        return route.abort();
      }
      state.passwordSubmitted = true;
    } else if (
      PAY_HOSTS.has(url.hostname) &&
      request.method() === "POST" &&
      !POST_PATHS.has(url.pathname)
    ) {
      state.unsupported = true;
      return route.abort();
    }
    // Cashier return pages are read-only; merchant pages are outside this worker.
    if (RETURN_HOSTS.has(url.hostname) && request.method() !== "GET")
      return route.abort();
    return route.continue();
  };
}

async function fillPassword(page: Page, password: string) {
  const fields = page.locator('input[type="password"]:visible');
  if ((await fields.count()) !== 1) return false;
  await fields.fill(password);
  return true;
}

async function runAttempt(rawInput: unknown): Promise<PaymentResult> {
  const state: AttemptState = {
    passwordSubmitted: false,
    debitSubmitted: false,
    authArmed: false,
    commitArmed: false,
  };
  let input: PaymentInput;
  let browser: Browser | undefined;
  let context: BrowserContext | undefined;
  let relay: Awaited<ReturnType<typeof startRelay>> | undefined;
  let timer: ReturnType<typeof setTimeout> | undefined;
  try {
    input = validateInput(rawInput);
  } catch {
    return result(state, "invalid_order");
  }
  const timeoutMs = 75000;
  const expires = Date.now() + timeoutMs;
  let timedOut = false;
  try {
    relay = await startRelay(input.proxy);
    browser = await chromium.launch({
      headless: true,
      executablePath: process.env.CC_PAY_BROWSER || undefined,
      timeout: 15000,
      proxy: { server: relay.server, bypass: "<-loopback>" },
      args: [
        "--disable-dev-shm-usage",
        "--disable-quic",
        "--force-webrtc-ip-handling-policy=disable_non_proxied_udp",
      ],
    });
    timer = setTimeout(() => {
      timedOut = true;
      browser?.close().catch(() => {});
    }, timeoutMs);
    context = await browser.newContext({
      locale: "zh-CN",
      serviceWorkers: "block",
      acceptDownloads: false,
    });
    await context.route("**/*", createGate(input, state));
    await context.routeWebSocket("**/*", (socket) => socket.close());
    const page = await context.newPage();
    page.setDefaultTimeout(5000);
    context.on("page", (popup) => {
      if (popup !== page) popup.close().catch(() => {});
    });
    page.on("dialog", (dialog) => dialog.dismiss().catch(() => {}));
    page.on("response", async (response) => {
      try {
        const url = new URL(response.url());
        if (
          url.hostname === AUTH_HOST &&
          url.pathname === AUTH_PATH &&
          providerError(await response.json())
        ) {
          state.rejected = true;
        }
      } catch {
        /* Never log provider responses or URLs. */
      }
    });
    await page.goto(input.action, {
      waitUntil: "domcontentloaded",
      timeout: 20000,
    });
    let switched = false,
      loginFilled = false,
      authClicked = false,
      commitClicked = false;
    while (Date.now() < expires && !timedOut) {
      const url = new URL(page.url());
      if (url.searchParams.get("errorCode") || state.rejected) {
        state.rejected = true;
        return result(state, "provider_rejected");
      }
      if (state.invalidComponent) return result(state, "component_unavailable");
      if (state.unsupported) return result(state, "interactive_required");
      if (RETURN_HOSTS.has(url.hostname))
        return result(state, "confirmation_required");
      if (
        url.hostname === AUTH_HOST &&
        (await page.locator("#J_changePayStyle").isVisible())
      ) {
        if (!switched) {
          switched = true;
          await page.locator("#J_changePayStyle").click();
        }
      } else if (
        url.hostname === AUTH_HOST &&
        (await page.locator("#J_TloginForm").isVisible())
      ) {
        const login = page.locator('#J_TloginForm input[name="loginId"]');
        if (!loginFilled && (await login.isVisible())) {
          await login.fill(input.login);
          await login.press("Tab"); // Official code renders the live security components on blur.
          loginFilled = true;
        }
        const componentReady = await page.evaluate(() => {
          const provider = window as Window & {
            light?: { page?: { products?: { payPasswd?: unknown } } };
            json_ua?: unknown;
          };
          return Boolean(
            provider.light?.page?.products?.payPasswd &&
            provider.json_ua &&
            document.querySelector<HTMLInputElement>('input[name="securityId"]')?.value,
          );
        });
        if (
          loginFilled &&
          !authClicked &&
          componentReady &&
          (await fillPassword(page, input.password))
        ) {
          const next = page.locator("#J_newBtn");
          if ((await next.isVisible()) && (await next.isEnabled())) {
            authClicked = true;
            state.authArmed = true;
            await next.click();
          }
        }
        if (
          authClicked &&
          !state.passwordSubmitted &&
          (await page
            .getByText(
              /支付密码错误|支付密码不正确|请先完成验证|请输入校验码|请输入验证码/,
            )
            .first()
            .isVisible())
        ) {
          return result(state, "interactive_required");
        }
      } else if (
        PAY_HOSTS.has(url.hostname) &&
        url.pathname.startsWith("/business/") &&
        !commitClicked
      ) {
        // The known Alipay cashier's exact confirmation action. No login-password,
        // OTP, agreement, credit, or alternate payment method is selected here.
        if (
          await page
            .getByText(/请输入短信验证码|请输入校验码|请完成安全验证/)
            .first()
            .isVisible()
        ) {
          return result(state, "interactive_required");
        }
        const passwords = page.locator('input[type="password"]:visible');
        if ((await passwords.count()) === 1)
          await passwords.fill(input.password);
        const confirm = page
          .getByRole("button", { name: /^(确认付款|确认支付|立即付款)$/ })
          .or(
            page.getByRole("link", { name: /^(确认付款|确认支付|立即付款)$/ }),
          );
        if (
          (await confirm.count()) === 1 &&
          (await confirm.isVisible()) &&
          (await confirm.isEnabled())
        ) {
          commitClicked = true;
          state.commitArmed = true;
          await confirm.click();
        }
      }
      if (state.debitSubmitted) {
        // Campus payment is authoritative. Once sent, do not click or retry anything.
        await delay(Math.min(1500, Math.max(0, expires - Date.now())));
        return result(state, "confirmation_required");
      }
      await delay(200);
    }
    return result(state, "timeout");
  } catch {
    return result(state, timedOut ? "timeout" : "browser_unavailable");
  } finally {
    clearTimeout(timer);
    if (input) {
      input.password = "";
      input.login = "";
    }
    await context?.close().catch(() => {});
    await browser?.close().catch(() => {});
    await relay?.close().catch(() => {});
  }
}

async function selfTest() {
  const browser = await chromium.launch({
    headless: true,
    executablePath: process.env.CC_PAY_BROWSER || undefined,
  });
  try {
    const page = await browser.newPage();
    await page.setContent(
      '<title>payment-worker</title><input type="password">',
    );
    if ((await page.title()) !== "payment-worker") throw new Error("probe");
    const relay = await startRelay(null);
    await relay.close();
    return { protocol: 1, ready: true };
  } finally {
    await browser.close();
  }
}

if (import.meta.main) {
  // Child exit also closes the browser's debugging pipe. A bounded watchdog and
  // Rust process-group cleanup cover crashes and stalled browser shutdowns.
  const watchdog = setTimeout(() => process.exit(2), 90000);
  (async () => {
    if (process.argv[2] === "--self-test") return selfTest();
    const chunks = [];
    let length = 0;
    for await (const chunk of process.stdin) {
      length += chunk.length;
      if (length > 262144) throw new Error("input_size");
      chunks.push(chunk);
    }
    const buffer = Buffer.concat(chunks);
    let input;
    try {
      input = JSON.parse(buffer.toString("utf8"));
    } finally {
      buffer.fill(0);
      for (const chunk of chunks) chunk.fill(0);
    }
    return runAttempt(input);
  })()
    .then((output) => {
      clearTimeout(watchdog);
      process.stdout.write(JSON.stringify(output));
    })
    .catch(() => {
      clearTimeout(watchdog);
      process.stdout.write(
        JSON.stringify({
          protocol: 1,
          outcome: "uncertain",
          password_submitted: false,
          debit_submitted: false,
          reason: "worker_failure",
        }),
      );
      process.exitCode = 1;
    });
}
