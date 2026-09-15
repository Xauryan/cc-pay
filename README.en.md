# cc-pay

**A Rust API library for campus payments · WeChat, Alipay and e-CNY**

[简体中文](README.md) · [English](README.en.md)

![Rust](https://img.shields.io/badge/Rust-2024-000000?logo=rust)
![Tokio](https://img.shields.io/badge/Async-Tokio-463D5E)
![Reqwest](https://img.shields.io/badge/HTTP-Reqwest%20%2B%20Rustls-009688)
![Serde](https://img.shields.io/badge/JSON-Serde-E57324)
![TypeScript](https://img.shields.io/badge/TypeScript-strict-3178C6?logo=typescript&logoColor=white)
![Playwright](https://img.shields.io/badge/Browser-Playwright-2EAD33)
![Node.js](https://img.shields.io/badge/Node.js-%3E%3D24.12-5FA04E?logo=nodedotjs&logoColor=white)

Starting with an existing campus cashier order and authenticated session, cc-pay handles payment channels, QR codes, proxies and payment status. Embed it in a Rust service or supply your own HTTP client and attempt store through traits.

## Features

| Method | Capabilities | Runtime |
| --- | --- | --- |
| WeChat | QR code generation and order queries | Rust |
| Alipay | QR payments; optional password payments with QR fallback | Rust; Node.js + Chromium for password payments |
| e-CNY | Bound sub-wallet payments; one selected wallet or a single ordered pass | Rust |

- Cookie session reuse and authentication through an existing CAS SSO session.
- HTTP / HTTPS / SOCKS5 / SOCKS5H proxies, including credentials and private networks.
- Exact decimal amount checks, persistent duplicate-payment protection and uncertain-outcome handling.
- Isolated Alipay browser contexts, with credentials passed through stdin.

The library does not create merchant orders, generate merchant signatures or log in to institutional accounts. Automatic Alipay payments depend on the official security component, account status and any additional verification requirements.

## Installation

Use a stable toolchain supporting Rust 2024 edition. The package is available through Git and is not yet published to crates.io:

```toml
[dependencies]
cc-pay = { git = "https://github.com/Xauryan/cc-pay" }
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
anyhow = "1"
```

The Cargo package name is `cc-pay`; the Rust import name is `cc_pay`. Pin a Git commit with `rev` in production.

## Quick start

```rust,no_run
use cc_pay::{Client, PaymentMethod, PaymentOptions};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cashier = std::env::var("CC_PAY_CASHIER_URL")?;
    let cookie = std::env::var("CC_PAY_COOKIE")?;
    let client = Client::builder()
        .cookie_header("https://cashier.cc-pay.cn", cookie)
        .state_directory("./.cc-pay/attempts")
        .build()?;

    let transaction = client.transaction(&cashier).await?;
    println!("status={}", transaction["status"]);

    let payment = client.create_payment(
        &cashier,
        PaymentMethod::Wechat,
        PaymentOptions {
            expected_amount: Some("1.00"),
            ..Default::default()
        },
    ).await?;

    // qr_png contains a Base64-encoded PNG for your application to display.
    if let Some(png) = payment.qr_png {
        let _image_source = format!("data:image/png;base64,{png}");
    }
    Ok(())
}
```

Use `PaymentMethod::Alipay` for an Alipay QR payment. The caller reads the environment variables in these examples; the library does not load `.env`. Cookie headers use `name=value; other=value` syntax. Use `cookie_jar` to share an existing `Arc<CookieJar>`.

## Alipay password payments

Install the optional component only when using password payments. With **Node.js ≥ 24.12** and pnpm installed, run from the repository or extracted `.crate` directory:

```sh
cd payment-worker
pnpm install --frozen-lockfile
pnpm exec playwright-core install chromium --only-shell
pnpm check
```

Add `--with-deps` to the browser installation command if Linux system dependencies are missing. The worker uses [Node.js native TypeScript support](https://nodejs.org/api/typescript.html) and does not require generated JavaScript files. `pnpm check` only probes local browser startup; it does not initiate a payment.

```rust,no_run
use cc_pay::{AlipayCredentials, BrowserPayer, Client, PaymentMethod, PaymentOptions};

# async fn example() -> anyhow::Result<()> {
let client = Client::builder()
    .cookie_header("https://cashier.cc-pay.cn", std::env::var("CC_PAY_COOKIE")?)
    .alipay_browser(BrowserPayer::default())
    .build()?;
let cashier = std::env::var("CC_PAY_CASHIER_URL")?;
let account = std::env::var("CC_PAY_ALIPAY_ACCOUNT")?;
let password = std::env::var("CC_PAY_ALIPAY_PASSWORD")?;
let payment = client.create_payment(&cashier, PaymentMethod::Alipay, PaymentOptions {
    alipay: Some(AlipayCredentials { account: &account, password: &password }),
    expected_amount: Some("1.00"),
    ..Default::default()
}).await?;
# Ok(())
# }
```

| Worker environment variable | Purpose |
| --- | --- |
| `CC_PAY_NODE` | Node.js executable; defaults to `node` |
| `CC_PAY_WORKER` | Absolute path to `worker.ts`; defaults to the package directory at compile time |
| `CC_PAY_BROWSER` | Existing Chromium executable; allows skipping the browser download |

When deploying a binary, also deploy `payment-worker` with its dependencies and set `CC_PAY_WORKER`. Without Alipay credentials, the library uses QR payments. Explicit rejection or a pre-submission failure can fall back to QR after checking the order. Submitted or uncertain payments only trigger status queries.

## e-CNY

First bind the merchant sub-wallet in the e-CNY app. Select `PaymentMethod::Ecny` and set `PaymentOptions.ecny_wallet_index`:

- `0`: try each wallet at most once, in the `ecCode` order returned by `payment_ways`.
- `1..=99`: try only that wallet index; stop before debit if it does not exist.

Wallets change only after an explicit rejection and confirmation that the order remains unpaid. Timeouts and uncertain outcomes stop the sequence. The campus order confirms success; e-CNY payments do not produce QR codes.

## Sessions and proxies

```rust,no_run
use cc_pay::Client;

# async fn example() -> anyhow::Result<()> {
let client = Client::builder()
    .proxy("socks5h://user:password@127.0.0.1:1080")
    .sso_origin("https://sso.example.org")
    .cookie_header("https://sso.example.org", "SESSION=existing-session")
    .build()?;
let cashier = std::env::var("CC_PAY_CASHIER_URL")?;
client.authenticate(&cashier, "https://sso.example.org/login").await?;
# Ok(())
# }
```

The proxy applies to both Rust requests and the Alipay browser, with no direct fallback on failure. Requests are restricted to approved HTTPS origins on port 443. Transparent retries and automatic redirects are disabled; the payment flow follows only validated redirects.

## Payment results and duplicate protection

| Result | Caller action |
| --- | --- |
| `status == "success"` | Campus cashier has confirmed payment |
| `needs_confirmation() == true` | Only query `transaction`; reconcile manually if necessary |
| `qr_png` is present | Display the QR code, then query the order |
| `automatic.outcome == "rejected"` | Automatic payment was explicitly rejected; display the QR code if present |
| Other states or errors | Query the order or handle manually; avoid blind retries |

`Client::builder()` stores automatic-payment claims in `.cc-pay/attempts` by default. Each order can start only one automatic-payment attempt; claims are never automatically released. Processes must share the same local directory. Multiple servers should implement `AttemptStore` using shared storage with a unique constraint. The advanced `Client::new` constructor uses an in-memory store and requires explicit persistence configuration.

**When migrating from an earlier version**, update the package name, imports and worker environment variables. Keep using the existing attempt directory through `state_directory`, or migrate its records while the application is stopped. Never clear claims to retry an unconfirmed payment. Do not log passwords, cookies, signed URLs or raw payment responses.

## API overview

| Interface | Purpose |
| --- | --- |
| `Client::builder()` / `PaymentClient` | Default client, sessions, proxies and persistent claims |
| `transaction` / `payment_ways` | Query orders and available channels |
| `authenticate` | Authenticate with an existing CAS SSO session |
| `create_payment` | Create a QR payment or initiate an automatic payment |
| `BrowserPayer` / `PasswordPayer` | Built-in Alipay worker or custom implementation |
| `Transport` / `AttemptStore` | Custom HTTP transport and shared attempt storage |

## Layout and verification

```text
src/              Rust API, payment flows, sessions and attempt storage
payment-worker/   Optional TypeScript Alipay password-payment worker
README.md         Chinese documentation
README.en.md      English documentation
```

```sh
cargo test --locked
pnpm --dir payment-worker typecheck
pnpm --dir payment-worker check
```

Inline unit tests cover core behavior, and Rust documentation examples are checked by the compiler. There are no standalone demo programs or platform-specific installation scripts.
