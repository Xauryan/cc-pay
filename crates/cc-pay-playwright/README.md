# cc-pay-playwright

A Rust Playwright adapter for Alipay password payments through [cc-pay](../../README.en.md).

`PlaywrightPayer` implements `cc_pay::PasswordPayer` using an application-owned
`playwright_rs::Browser`. Each payment uses a fresh, non-persistent context with
request validation and submission tracking. The adapter returns a
`PasswordPayment` for the core client to reconcile with the campus transaction.

## Dependencies

```toml
[dependencies]
cc-pay = { git = "https://github.com/Xauryan/cc-pay" }
cc-pay-playwright = { git = "https://github.com/Xauryan/cc-pay" }
playwright-rs = { version = "=0.18.1", default-features = false, features = ["ring", "rustls-tls-webpki-roots"] }
```

The adapter uses `playwright-rs 0.18.1`. Its runtime consists of the Playwright
driver, Node.js and a compatible Chromium. The host owns driver provisioning,
browser launch, proxy configuration, logging and shutdown.

## Build a payment client

Supply the browser, authenticated campus session and shared attempt store:

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

Keep the Playwright owner alive while the browser is in use. A dedicated payment
browser gives the host a separate place to apply payment network and logging
policies.

## Request a password payment

Pass the Alipay account, six-digit payment password and expected amount:

```rust,no_run
use cc_pay::{AlipayCredentials, Payment, PaymentClient, PaymentMethod, PaymentOptions};
use cc_pay_playwright::PlaywrightPayer;

pub async fn pay(
    client: &PaymentClient<PlaywrightPayer>,
    cashier_url: &str,
    account: &str,
    password: &str,
    expected_amount: &str,
) -> Result<Payment, Box<dyn std::error::Error>> {
    Ok(client.create_payment(cashier_url, PaymentMethod::Alipay, PaymentOptions {
        alipay: Some(AlipayCredentials { account, password }),
        expected_amount: Some(expected_amount),
        ..Default::default()
    }).await?)
}
```

For QR payment, set `PaymentOptions.alipay` to `None`. Automatic payment requires
an explicit `AttemptStore`; the core client records the attempt before calling
the adapter.

## Concurrency and lifecycle

| Setting | Default | Configuration |
| --- | --- | --- |
| Concurrent attempts per payer | `1` | `concurrency(NonZeroUsize)` |
| Capacity wait timeout | 60 seconds | First argument of `timeouts(queue, attempt)` |
| Context creation and payment timeout | 75 seconds total | Second argument of `timeouts(queue, attempt)` |

Configure the payer before sharing it. Clones share a concurrency limit, while
separately constructed payers have their own limits. Context creation and cleanup
hold a capacity permit until they finish or the bounded cleanup wait expires.

Task cancellation closes the request gate immediately and schedules context
cleanup. Contexts created after a cancelled or timed-out request are also
scheduled for cleanup. The host owns the supplied browser and its other contexts.
Keep the Tokio runtime alive until payment tasks and cleanup have finished, then
close the browser through the host's lifecycle management.

## Network configuration

The core client's `proxy` setting applies to HTTP requests. Configure the supplied
browser's proxy separately and apply a consistent egress policy to both clients.
Authenticated SOCKS5 browser connections require an application-owned proxy relay.

The host's restricted proxy or firewall must enforce HTTPS port 443 destinations
for browser traffic, including redirect chains:

- `alipay.com` and its subdomains;
- `alipayobjects.com` and its subdomains;
- `alipaylog.com` and its subdomains;
- `cashier.cc-pay.cn`.

Launch the browser with QUIC disabled and WebRTC restricted to proxied traffic,
using `--disable-quic` and
`--force-webrtc-ip-handling-policy=disable_non_proxied_udp`. Apply an egress policy
that stops traffic when the configured proxy is unavailable.

## Payment validation and outcomes

Before opening a context, the adapter checks the Alipay gateway URL, presence
of the signature field, API method, order fields, amount, account and
payment-password format.
The browser request gate controls gateway submission, permitted POST paths,
password submission and debit submission. The official page's security component
handles password encryption; the gate checks the outgoing component fields.

Each context blocks service workers and WebSockets, dismisses dialogs and closes
popups. Password and debit requests are each admitted at most once by the gate.
Debit dispatch is recorded before transport so that subsequent failures preserve
the pending-confirmation state.

| `PasswordOutcome` | Meaning |
| --- | --- |
| `NotSubmitted` | The adapter stopped before a debit request was dispatched |
| `Rejected` | The provider rejected the attempt before debit dispatch |
| `Uncertain` | A debit was dispatched or the result requires confirmation |

The core client queries the campus transaction to confirm payment. A pending
result permits status queries; QR fallback requires a safe adapter outcome and
a campus order that remains unpaid.

The host's logs, browser protocol traces, HAR files, videos and debug output
should exclude credentials, signed URLs and raw provider responses. The adapter
maps browser failures to payment reasons and zeroizes its owned account and form
buffers where applicable. Driver and browser credential copies follow the host's
runtime lifecycle.

## Development checks

Run from the repository root. Set `PLAYWRIGHT_SKIP_DRIVER_DOWNLOAD=1` to skip
the driver download during compilation:

```sh
PLAYWRIGHT_SKIP_DRIVER_DOWNLOAD=1 cargo test --locked --workspace
PLAYWRIGHT_SKIP_DRIVER_DOWNLOAD=1 cargo clippy --locked --workspace --all-targets -- -D warnings
```

The default suite covers order validation, request policy, submission state and
Rust documentation examples.

After provisioning the binding's driver and a compatible Chromium, run the local
browser fixtures:

```sh
CC_PAY_TEST_BROWSER=/absolute/path/to/chromium cargo test --locked -p cc-pay-playwright --lib -- --ignored
```

`CC_PAY_TEST_BROWSER` selects the executable for these opt-in tests. With the
variable unset, tests use the binding's installed Chromium. Fixtures fulfill or
abort page requests and block DNS for unmocked destinations. They exercise
password entry, request gating, a simulated debit, cancellation and late-context
cleanup. Real account and provider acceptance is performed separately in the
host's integration environment.
