# cc-pay

**A campus-payment API library for other Rust projects to import and reference.**

[简体中文](README.md) · [English](README.en.md)

cc-pay provides WeChat QR payments, Alipay QR and password payments, e-CNY wallet
payments and transaction queries. The caller supplies an existing campus cashier
order and authenticated session. The host application configures the runtime,
proxies, storage and result presentation.

## Packages and features

| Package | Features | Integration |
| --- | --- | --- |
| `cc-pay` | Sessions, HTTP APIs, QR codes, e-CNY, attempt claims and reconciliation | Build a `Client` and supply session data and payment options |
| `cc-pay-playwright` | Alipay password payments | Supply a running `Browser` and inject the adapter through `PasswordPayer` |

`cc-pay` can be used independently. See [cc-pay-playwright](crates/cc-pay-playwright/README.md)
for browser adapter configuration and runtime requirements.

## Dependency

Use a toolchain supporting Rust 2024 edition and add a Git dependency:

```toml
[dependencies]
cc-pay = { git = "https://github.com/Xauryan/cc-pay" }
anyhow = "1"
```

The Cargo package is `cc-pay` and the Rust import is `cc_pay`. Production
integrations can pin a commit with `rev`. Async requests run on the host's Tokio
runtime.

## Create a QR payment

The following example accepts a cashier URL, Cookie request header and expected
amount. Call it from the host's Tokio runtime:

```rust,no_run
use cc_pay::{Client, Payment, PaymentMethod, PaymentOptions};

pub async fn wechat_payment(
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

`Payment.qr_png` contains a Base64 PNG that the host can display as a
`data:image/png;base64,...` URL. For Alipay QR payments, use
`PaymentMethod::Alipay` with `PaymentOptions.alipay` set to `None`.

## Client and sessions

`Client::builder()` uses the built-in `HttpTransport`. The type parameter `P` in
`ClientBuilder<P>` and `PaymentClient<P>` represents the password payer and
defaults to `NoPasswordPayer`.

| Configuration method | Purpose |
| --- | --- |
| `cookie_jar(jar)` | Share the host's `Arc<CookieJar>` |
| `cookie_header(origin, header)` | Add cookies in `name=value; other=value` format for an origin |
| `sso_origin(origin)` | Allow a CAS SSO origin |
| `proxy(url)` | Configure the HTTP transport's proxy |
| `user_agent(value)` | Set the HTTP User-Agent |
| `attempt_store(store)` | Inject an `Arc<dyn AttemptStore>` |
| `state_directory(path)` | Store payment claims in a local directory |
| `password_payer(payer)` | Inject a `PasswordPayer` implementation |

Cookie and SSO origins use HTTPS port 443, a `/` path and empty query and fragment
components. The host supplies an authenticated session;
`authenticate(cashier_url, sso_login_url)` reuses it for campus SSO redirects.

```rust,no_run
use cc_pay::{Client, CookieJar, PaymentClient};
use std::sync::Arc;

pub fn payment_client(jar: Arc<CookieJar>, proxy: &str) -> anyhow::Result<PaymentClient> {
    Client::builder()
        .cookie_jar(jar)
        .proxy(proxy)
        .sso_origin("https://sso.example.org")
        .build()
}
```

`HttpTransport` supports authenticated HTTP, HTTPS, SOCKS5 and SOCKS5H proxies.
Proxy configuration is explicit, and proxy failures return errors. The connection
timeout is 5 seconds, the request timeout is 15 seconds and the response body
limit is 8 MiB. Destinations are restricted to approved HTTPS port 443 addresses.
Automatic retries and redirects are disabled; the payment flow validates and
follows redirects individually.

The HTTP client and browser have separate proxy configuration. The host applies
a consistent egress policy to both.

## Automatic payments and attempt storage

**Alipay password payments and e-CNY require an explicitly configured
`AttemptStore`.** When storage is missing, `create_payment` returns an error
before making a network request. Queries and QR payments can use the default
client.

```rust,no_run
use cc_pay::{AttemptStore, Client, PaymentClient};
use std::sync::Arc;

pub fn with_shared_store(store: Arc<dyn AttemptStore>) -> anyhow::Result<PaymentClient> {
    Client::builder().attempt_store(store).build()
}
```

| Storage | Use case | Behavior |
| --- | --- | --- |
| Custom `AttemptStore` | Multiple application hosts | Atomically persist order claims in shared storage with a unique constraint |
| `FileAttemptStore` / `state_directory(path)` | Application processes sharing a local directory | Create claim files atomically and retain them across restarts |
| `MemoryAttemptStore` | Tests or process-local protection | Retain claims in the shared store instance for the lifetime of the process |

`AttemptStore::contains` checks for a claim, and `claim` returns `true` at most
once per order. Persistent implementations must durably record the claim before
returning success. The last call to `attempt_store` or `state_directory` selects
the storage configuration. Local directories are created during `build()`.

Automatic-payment claims are stored by order and block subsequent payment
creation requests for that order across payment methods. Claims remain recorded.
After query failures, task cancellation or uncertain results, limit subsequent
actions to transaction queries and reconciliation. Applications should retain
the same claims across restarts and share them across instances.

`PaymentOptions.expected_amount` accepts a decimal string. Amount comparisons use
integer minor units. The expected amount is checked before automatic debit, and
a mismatch stops the debit.

## e-CNY

Bind the merchant sub-wallet in the e-CNY app, then use `PaymentMethod::Ecny`.
`PaymentOptions.ecny_wallet_index` controls wallet selection:

| Value | Behavior |
| --- | --- |
| `0`, the default | Try each wallet once in the `ecCode` order returned by `payment_ways` |
| `1..=99` | Try the selected wallet only; an index beyond the bound wallet count stops preparation |

Wallet rotation requires an explicit rejection and confirmation that the same
order remains unpaid. Timeouts and uncertain outcomes stop subsequent attempts.
The campus transaction status confirms successful payment.

## Alipay password payments

Add `cc-pay-playwright`, pass the host's `playwright_rs::Browser` to
`PlaywrightPayer::new(browser)` and inject it with `password_payer`. When calling
`create_payment`, supply the Alipay account and six-digit payment password through
`AlipayCredentials` in `PaymentOptions.alipay`.

The adapter uses `playwright-rs 0.18.1`; its runtime includes the Playwright driver,
Node.js and Chromium. The host manages provisioning, browser launch, proxies,
logging and shutdown. See the [adapter README](crates/cc-pay-playwright/README.md)
for complete examples and configuration requirements.

## Result handling

`create_payment` returns a `Payment` containing the method, order status, amount,
QR code and automatic-payment result.

| Result | Application action |
| --- | --- |
| `status == "success"` | Campus cashier confirmed payment |
| `needs_confirmation()` returns `true` | Query `transaction` and reconcile manually if necessary |
| `qr_png` is present | Display the QR code and query order state |
| `automatic.outcome == "rejected"` | Automatic payment was explicitly rejected; display the QR code if present |
| Error or another state | Query the order and reconcile before taking further action |

`automatic.debit_submitted` records debit dispatch, and `password_submitted`
records password-request dispatch. Host logs, browser traces and HAR files should
exclude passwords, cookies, signed URLs and raw provider responses.

## Queries and extension interfaces

`transaction(cashier_url)` queries the order, and `payment_ways(cashier_url)`
queries available payment channels. Both return `serde_json::Value` for access
to upstream fields.

| Interface | Contract |
| --- | --- |
| `Transport` | Share cookies, validate destinations, bound responses and disable automatic retries and redirects |
| `AttemptStore` | Query and atomically record automatic-payment attempts; storage errors stop payment |
| `PasswordPayer` | Accept a signed form, amount and credentials, and return `PasswordPayment` |

`Client::new(transport)` accepts a custom transport.
`Client::with_password_payer(transport, payer)` also accepts a custom payer.
Configure claims through `with_attempt_store(store)`. `PasswordPayer` supports
`Arc<P>` and `Option<P>` wrappers.

`PasswordPayment` provides `not_submitted`, `rejected` and `uncertain`
constructors, plus accessors for the outcome, reason and submission flags. Return
`uncertain` when a debit may have been dispatched. The campus transaction state
confirms final success.

## Development checks

Run from the repository root:

```sh
cargo test --locked -p cc-pay
PLAYWRIGHT_SKIP_DRIVER_DOWNLOAD=1 cargo test --locked --workspace
PLAYWRIGHT_SKIP_DRIVER_DOWNLOAD=1 cargo clippy --locked --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
```

The default suite uses simulated APIs to cover payment state, claims, amount
validation and routing policy. Rust documentation examples are compiled as part
of testing. Local browser tests use in-memory pages and fulfilled responses; see
the adapter README for instructions. Real account and provider acceptance is
performed separately in the host's integration environment.
