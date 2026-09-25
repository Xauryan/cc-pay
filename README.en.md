# cc-pay

**A campus-payment API library for other Rust projects to import and reference.**

[简体中文](README.md) · [English](README.en.md)

The library starts with an existing campus cashier order and authenticated session.
It supports WeChat, Alipay, e-CNY and transaction queries. The consuming application
owns configuration, logging, runtime and deployment. This repository provides no
CLI or standalone service.

## Packages

| Package | Responsibility | Boundary |
| --- | --- | --- |
| `cc-pay` | Sessions, HTTP APIs, QR payments, e-CNY, attempt claims and reconciliation | No Playwright, Node or script dependency |
| `cc-pay-playwright` | Optional Rust implementation of `PasswordPayer` | Application supplies a running `Browser` |

Importing `cc-pay` does not build the browser adapter. Merchant order creation,
signing and institutional interactive login are outside the library's scope.

## Dependency and integration

The packages are not published to crates.io. Use a Git dependency and pin `rev`
for production integrations:

```toml
[dependencies]
cc-pay = { git = "https://github.com/Xauryan/cc-pay" }
```

The Rust import is `cc_pay`. Use a Rust 2024 toolchain; the optional Playwright
binding requires Rust 1.88 or newer.

```rust,no_run
use cc_pay::{Client, Payment, PaymentMethod, PaymentOptions};

pub async fn prepare_payment(
    cashier_url: &str,
    cookie_header: &str,
    expected_amount: &str,
) -> anyhow::Result<Payment> {
    let client = Client::builder()
        .cookie_header("https://cashier.cc-pay.cn", cookie_header)
        .build()?;
    client.create_payment(cashier_url, PaymentMethod::Wechat, PaymentOptions {
        expected_amount: Some(expected_amount),
        ..Default::default()
    }).await
}
```

`qr_png` contains a Base64 PNG for the host to display. Select
`PaymentMethod::Alipay` without credentials for Alipay QR payments. Use
`transaction` and `payment_ways` to query order state and available channels.
These examples accept application parameters; no environment variables, working
directory convention or executable entry point are required.

## Sessions and transport

`cookie_header` accepts `name=value; other=value`. `cookie_jar` shares an existing
`Arc<CookieJar>`. Explicitly allow a CAS origin with `sso_origin` before calling
`authenticate` to reuse an existing SSO session.

`HttpTransport` supports authenticated HTTP, HTTPS, SOCKS5 and SOCKS5H proxies,
including private networks. It never inherits system proxy variables or retries
directly. It restricts requests to approved HTTPS port 443 destinations, bounds
responses and disables automatic retries/redirects. `Client::new(transport)`
accepts custom implementations respecting the `Transport` contract.

## Explicit automatic-payment storage

The default client does not create a directory. **Automatic Alipay and e-CNY
payments require an explicitly configured `AttemptStore`**, checked before any
network request:

```rust,no_run
use cc_pay::{AttemptStore, Client, PaymentClient};
use std::sync::Arc;

pub fn shared_client(store: Arc<dyn AttemptStore>) -> anyhow::Result<PaymentClient> {
    Client::builder().attempt_store(store).build()
}
```

Implement `AttemptStore` with an atomic persistent unique key for multiple hosts.
Opt into local storage with `.state_directory(application_owned_path)` or
`FileAttemptStore`. Explicitly inject `MemoryAttemptStore` only when process-local
protection is appropriate; it does not survive restarts.

Claims are never automatically released. Cancellation, errors and uncertain
outcomes must not cause another debit. `PaymentOptions.expected_amount` checks
exact decimal values before automatic debit.

For e-CNY, first bind the merchant sub-wallet in the official app. Select
`PaymentMethod::Ecny`. `ecny_wallet_index = 0` tries each upstream wallet once in
order; `1..=99` selects only that wallet. Rotation requires explicit rejection
and confirmation that the same order remains unpaid. Uncertainty stops the flow.

## Optional Alipay password adapter

Import `cc-pay-playwright`, pass an application-owned `playwright_rs::Browser` to
`PlaywrightPayer::new(browser)` and inject it through `.password_payer(payer)`.
See the [adapter README](crates/cc-pay-playwright/README.md) for a compiling
integration example and runtime/proxy responsibilities.

The core depends only on `PasswordPayer` and `PasswordPayment`, so other adapters
need no Playwright installation. Adapter results distinguish not submitted,
explicitly rejected and uncertain. Only campus transaction state confirms success.

Rust Playwright bindings still use a Node.js driver internally. This project no
longer contains a TypeScript worker, stdin/stdout protocol, compile-time script
path, or `CC_PAY_NODE` / `CC_PAY_WORKER` / `CC_PAY_BROWSER` settings. The optional
adapter uses `playwright-rs 0.18.1` for request interception. It does not install
or launch a driver/browser during payment calls.

## Result handling

| Result | Application action |
| --- | --- |
| `status == "success"` | Campus cashier confirmed payment |
| `needs_confirmation()` | Query `transaction`; reconcile if needed |
| `qr_png` is present | Display it and query order status |
| `automatic.outcome == "rejected"` | Explicit rejection; display QR if present |
| Error or another state | Query and reconcile; avoid blind retries |

Never log passwords, cookies, signed URLs or raw provider responses. Host browser
trace, HAR and debug logs can also disclose secrets.

## Migration from 0.2

1. Replace `BrowserPayer`, `BrowserResult` and `.alipay_browser(...)` with
   `PlaywrightPayer` or a custom `PasswordPayer`, returning `PasswordPayment`.
2. `ClientBuilder<P>` and `PaymentClient<P>` preserve the injected payer type;
   the default is `NoPasswordPayer`.
3. Default `.cc-pay/attempts` writes are removed. **Explicitly retain the existing
   directory or shared store**; never clear claims during migration.
4. `.proxy(...)` configures HTTP only. Configure the same browser exit in the
   host; authenticated SOCKS requires an application-owned relay. Remove worker
   deployment steps.

## Validation

```sh
cargo test --locked -p cc-pay
PLAYWRIGHT_SKIP_DRIVER_DOWNLOAD=1 cargo test --locked --workspace
PLAYWRIGHT_SKIP_DRIVER_DOWNLOAD=1 cargo clippy --locked --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
```

Default tests use simulated APIs and routing policies without real payments.
Optional local browser tests use in-memory pages and fulfilled routes; see the
adapter README. Passing tests does not establish real provider acceptance.
