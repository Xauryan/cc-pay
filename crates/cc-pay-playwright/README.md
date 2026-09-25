# cc-pay-playwright

Optional Rust Playwright adapter for `cc-pay`. The application supplies a running
`playwright_rs::Browser`; the adapter creates and closes a fresh context for each
payment. It never launches Node, a driver or a browser, and contains no CLI.

`PlaywrightPayer::new(browser)` implements `cc_pay::PasswordPayer`. Inject it with
`Client::builder().password_payer(payer)` and explicitly configure an `AttemptStore`.
The core `cc-pay` package has no dependency on this package or Playwright.

The application owns driver installation, browser launch, proxies, logging and
shutdown. Use a dedicated browser with QUIC and non-proxied WebRTC disabled. Keep
the Tokio runtime alive until all payment tasks and context cleanup complete.
The adapter closes its contexts, never the supplied browser. Clones share a
per-payer concurrency limit (default 1); timeouts can be configured.

Use the same network policy for the HTTP client and the supplied browser. The
core builder's `proxy` configures only HTTP. Browser proxy credentials and relays
are application-owned; Chromium does not directly support authenticated SOCKS5.
Applications using that proxy mode must supply an authenticated proxy relay.
No implicit direct-connection retry is made by this adapter.

This adapter uses `playwright-rs = 0.18.1`, whose driver still contains Node.js.
The older `playwright = 0.0.20` API does not expose context request interception.
The adapter needs context routing to enforce signed-order validation, one password
submission and at most one debit submission. POST destinations are allowlisted,
service workers and WebSockets are blocked, dialogs dismissed and popups closed.
Once a debit may have been sent, only campus transaction queries can confirm it.

Do not enable Playwright protocol tracing, HAR/video capture or verbose browser
logs in a payment environment: they may contain secrets. The adapter neither
returns driver errors nor logs credentials, URLs or provider responses. Rust
buffers owned by the adapter are zeroized where practical; the driver/browser
also holds credential copies and must be managed by the host.

Compile and test without downloading a driver:

```sh
PLAYWRIGHT_SKIP_DRIVER_DOWNLOAD=1 cargo test --workspace
```

Unit tests do not contact payment providers or establish real payment acceptance.

## Host integration

```toml
[dependencies]
cc-pay = { git = "https://github.com/Xauryan/cc-pay" }
cc-pay-playwright = { git = "https://github.com/Xauryan/cc-pay" }
playwright-rs = { version = "=0.18.1", default-features = false, features = ["ring", "rustls-tls-webpki-roots"] }
```

```rust,no_run
use cc_pay::{AttemptStore, Client, CookieJar, PaymentClient};
use cc_pay_playwright::PlaywrightPayer;
use playwright_rs::Browser;
use std::sync::Arc;

pub fn payment_client(
    browser: Browser,
    session: Arc<CookieJar>,
    claims: Arc<dyn AttemptStore>,
) -> Result<PaymentClient<PlaywrightPayer>, Box<dyn std::error::Error>> {
    Ok(Client::builder()
        .cookie_jar(session)
        .attempt_store(claims)
        .password_payer(PlaywrightPayer::new(browser))
        .build()?)
}
```

The host passes `PaymentOptions.alipay` to opt into password payment. Leave it
unset for QR payment. Keep the Playwright owner alive while the browser is used.

## Local browser verification

After the host has provisioned the binding's driver and a compatible Chromium:

```sh
CC_PAY_TEST_BROWSER=/absolute/path/to/chromium cargo test -p cc-pay-playwright --lib -- --ignored
```

`CC_PAY_TEST_BROWSER` is read only by opt-in tests. Omit it to use the binding's
installed Chromium. Tests abort or fulfill all page requests with fixtures and
block DNS for any unmocked destinations. They exercise password entry, the Rust
request gate, a single simulated debit, cancellation and late-context cleanup.
They do not contact Alipay or use real accounts.

The host must enforce the browser's network egress policy, including redirect
chains: allow only HTTPS port 443 to `alipay.com`, `alipayobjects.com`,
`alipaylog.com` and their subdomains, plus `cashier.cc-pay.cn`. Playwright routing
is a payment request guard, not a replacement for the host's egress firewall or
restricted proxy. The old worker's process-owned proxy relay is now a host
integration responsibility, as are authenticated SOCKS relays.
