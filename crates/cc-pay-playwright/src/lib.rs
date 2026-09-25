#![doc = include_str!("../README.md")]
mod gate;

use cc_pay::{AlipayCredentials, Fields, PasswordPayer, PasswordPayment, PasswordReason};
use gate::{
    AUTH_HOST, AUTH_PATH, Decision, Gate, RETURN_HOST, pay_host, provider_error, valid_input,
};
use playwright_rs::protocol::route::ContinueOptions;
use playwright_rs::{Browser, BrowserContext, BrowserContextOptions, GotoOptions, Page, WaitUntil};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, oneshot};
use zeroize::Zeroizing;

/// Uses a browser owned and configured by the host application. Each payment has
/// a fresh non-persistent context. Clones share the same concurrency limit.
/// This adapter never starts a driver, installs a browser, or closes the browser.
#[derive(Clone)]
pub struct PlaywrightPayer {
    browser: Browser,
    capacity: Arc<Semaphore>,
    queue_timeout: Duration,
    attempt_timeout: Duration,
}
impl PlaywrightPayer {
    pub fn new(browser: Browser) -> Self {
        Self {
            browser,
            capacity: Arc::new(Semaphore::new(1)),
            queue_timeout: Duration::from_secs(60),
            attempt_timeout: Duration::from_secs(75),
        }
    }
    /// Set before sharing the payer; zero is rejected. Independent payers have
    /// independent limits, so share clones when they use the same browser pool.
    pub fn concurrency(mut self, limit: std::num::NonZeroUsize) -> Self {
        self.capacity = Arc::new(Semaphore::new(limit.get()));
        self
    }
    pub fn timeouts(mut self, queue: Duration, attempt: Duration) -> Self {
        self.queue_timeout = queue;
        self.attempt_timeout = attempt;
        self
    }
}

/// Abort routes synchronously on cancellation, then close only our context.
/// The application's runtime must remain alive to finish asynchronous cleanup.
struct ContextGuard {
    context: Option<BrowserContext>,
    state: Arc<Mutex<Gate>>,
    permit: Option<OwnedSemaphorePermit>,
}
impl ContextGuard {
    fn context(&self) -> &BrowserContext {
        self.context
            .as_ref()
            .expect("context is present until closed")
    }
    async fn close(&mut self) {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).closed = true;
        if matches!(
            tokio::time::timeout(Duration::from_secs(5), self.context().close()).await,
            Ok(Ok(()))
        ) {
            self.context = None;
            self.permit = None;
        }
    }
}
impl Drop for ContextGuard {
    fn drop(&mut self) {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).closed = true;
        if let Some(context) = self.context.take() {
            let permit = self.permit.take();
            if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                runtime.spawn(async move {
                    // Do not release concurrency capacity while cleanup is pending.
                    let _permit = permit;
                    let _ = tokio::time::timeout(Duration::from_secs(5), context.close()).await;
                });
            }
        }
    }
}

async fn create_context(
    browser: Browser,
    state: Arc<Mutex<Gate>>,
    permit: OwnedSemaphorePermit,
    deadline: tokio::time::Instant,
) -> Option<ContextGuard> {
    let (sender, receiver) = oneshot::channel();
    // A dropped RPC future can leave a remote context behind. Let the creation
    // finish even if its caller is cancelled; a failed send drops the guard and
    // closes that late context. Retain the permit until creation/cleanup finishes.
    tokio::spawn(async move {
        let options = BrowserContextOptions::builder()
            .locale("zh-CN".into())
            .service_workers("block".into())
            .accept_downloads(false)
            .build();
        let guard = browser
            .new_context_with_options(options)
            .await
            .ok()
            .map(|context| ContextGuard {
                context: Some(context),
                state,
                permit: Some(permit),
            });
        let _ = sender.send(guard);
    });
    tokio::time::timeout_at(deadline, receiver)
        .await
        .ok()?
        .ok()?
}

impl PasswordPayer for PlaywrightPayer {
    async fn pay(
        &self,
        action: &str,
        fields: &Fields,
        amount: &str,
        credentials: AlipayCredentials<'_>,
    ) -> PasswordPayment {
        if !valid_input(action, fields, amount, credentials) {
            return PasswordPayment::not_submitted(PasswordReason::InvalidOrder);
        }
        let Ok(Ok(permit)) =
            tokio::time::timeout(self.queue_timeout, self.capacity.clone().acquire_owned()).await
        else {
            return PasswordPayment::not_submitted(PasswordReason::Busy);
        };
        let state = Arc::new(Mutex::new(Gate::default()));
        let deadline = tokio::time::Instant::now() + self.attempt_timeout;
        let Some(mut guard) =
            create_context(self.browser.clone(), state.clone(), permit, deadline).await
        else {
            return PasswordPayment::not_submitted(PasswordReason::Unavailable);
        };
        let reason = match tokio::time::timeout_at(
            deadline,
            run(guard.context(), state.clone(), action, fields, credentials),
        )
        .await
        {
            Ok(Ok(reason)) => reason,
            Ok(Err(_)) => PasswordReason::Unavailable,
            Err(_) => PasswordReason::Timeout,
        };
        let result = {
            let mut gate = state.lock().unwrap_or_else(|e| e.into_inner());
            gate.closed = true;
            gate.result(reason)
        };
        guard.close().await;
        // Drop also covers task cancellation at any await above.
        result
    }
}

async fn run(
    context: &BrowserContext,
    state: Arc<Mutex<Gate>>,
    action: &str,
    fields: &Fields,
    credentials: AlipayCredentials<'_>,
) -> playwright_rs::Result<PasswordReason> {
    let route_state = state.clone();
    let action_owned = action.to_owned();
    let account = Arc::new(Zeroizing::new(credentials.account.to_owned()));
    let encoded = Arc::new(Zeroizing::new(
        url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs(fields)
            .finish(),
    ));
    context
        .route("**/*", move |route| {
            let state = route_state.clone();
            let action = action_owned.clone();
            let account = account.clone();
            let encoded = encoded.clone();
            async move {
                let request = route.request();
                let body = Zeroizing::new(request.post_data().unwrap_or_default());
                let decision = state.lock().unwrap_or_else(|e| e.into_inner()).decide(
                    request.url(),
                    request.method(),
                    request.is_navigation_request(),
                    &body,
                    &action,
                    &account,
                );
                match decision {
                    Decision::Abort => route.abort(None).await,
                    Decision::Continue => route.continue_(None).await,
                    Decision::Gateway => {
                        let mut headers = request.headers();
                        headers.insert(
                            "content-type".into(),
                            "application/x-www-form-urlencoded".into(),
                        );
                        route
                            .continue_(Some(
                                ContinueOptions::builder()
                                    .method("POST".into())
                                    .headers(headers)
                                    .post_data(encoded.to_string())
                                    .build(),
                            ))
                            .await
                    }
                }
            }
        })
        .await?;
    context
        .route_web_socket("**/*", |socket| async move { socket.close(None).await })
        .await?;
    context
        .on_dialog(|dialog| async move { dialog.dismiss().await })
        .await?;
    let response_state = state.clone();
    context
        .on_response(move |response| {
            let state = response_state.clone();
            async move {
                let relevant = url::Url::parse(response.url())
                    .is_ok_and(|u| u.host_str() == Some(AUTH_HOST) && u.path() == AUTH_PATH);
                if relevant
                    && let Ok(body) = response.body().await
                    && body.len() <= 1024 * 1024
                    && let Ok(value) = serde_json::from_slice::<serde_json::Value>(&body)
                    && provider_error(&value)
                {
                    state.lock().unwrap_or_else(|e| e.into_inner()).rejected = true;
                }
                Ok(())
            }
        })
        .await?;
    let page = context.new_page().await?;
    context
        .on_page(|popup| async move { popup.close().await })
        .await?;
    page.set_default_timeout(5000.0).await;
    page.goto(
        action,
        Some(
            GotoOptions::new()
                .wait_until(WaitUntil::DomContentLoaded)
                .timeout(Duration::from_secs(20)),
        ),
    )
    .await?;
    interact(&page, state, credentials).await
}

async fn interact(
    page: &Page,
    state: Arc<Mutex<Gate>>,
    credentials: AlipayCredentials<'_>,
) -> playwright_rs::Result<PasswordReason> {
    let (mut switched, mut login_filled, mut auth_clicked, mut commit_clicked) =
        (false, false, false, false);
    loop {
        let raw = page.url();
        let Ok(url) = url::Url::parse(&raw) else {
            return Ok(PasswordReason::Unavailable);
        };
        {
            let mut gate = state.lock().unwrap_or_else(|e| e.into_inner());
            if url
                .query_pairs()
                .any(|(k, v)| k == "errorCode" && !v.is_empty())
            {
                gate.rejected = true;
            }
            if gate.debit_submitted {
                return Ok(PasswordReason::ConfirmationRequired);
            }
            if gate.rejected {
                return Ok(PasswordReason::ProviderRejected);
            }
            if gate.invalid_component {
                return Ok(PasswordReason::ComponentUnavailable);
            }
            if gate.unsupported {
                return Ok(PasswordReason::InteractiveRequired);
            }
        }
        let host = url.host_str().unwrap_or("");
        if host == RETURN_HOST {
            return Ok(PasswordReason::ConfirmationRequired);
        }
        if host == AUTH_HOST && page.locator("#J_changePayStyle").is_visible().await? {
            if !switched {
                switched = true;
                page.locator("#J_changePayStyle").click(None).await?;
            }
        } else if host == AUTH_HOST && page.locator("#J_TloginForm").is_visible().await? {
            let login = page.locator("#J_TloginForm input[name=loginId]");
            if !login_filled && login.is_visible().await? {
                login.fill(credentials.account, None).await?;
                login.press("Tab", None).await?;
                login_filled = true;
            }
            // Only inspect readiness; the provider's own component encrypts the password.
            let ready: bool = page.evaluate("() => Boolean(window.light?.page?.products?.payPasswd && window.json_ua && document.querySelector('input[name=securityId]')?.value)", Some(&())).await?;
            if login_filled
                && !auth_clicked
                && ready
                && fill_password(page, credentials.password).await?
            {
                let next = page.locator("#J_newBtn");
                if next.is_visible().await? && next.is_enabled().await? {
                    auth_clicked = true;
                    state.lock().unwrap_or_else(|e| e.into_inner()).auth_armed = true;
                    next.click(None).await?;
                }
            }
            if auth_clicked
                && visible_text(
                    page,
                    &[
                        "支付密码错误",
                        "支付密码不正确",
                        "请先完成验证",
                        "请输入校验码",
                        "请输入验证码",
                    ],
                )
                .await?
            {
                return Ok(PasswordReason::InteractiveRequired);
            }
        } else if pay_host(host) && url.path().starts_with("/business/") && !commit_clicked {
            if visible_text(
                page,
                &["请输入短信验证码", "请输入校验码", "请完成安全验证"],
            )
            .await?
            {
                return Ok(PasswordReason::InteractiveRequired);
            }
            fill_password(page, credentials.password).await?;
            // Playwright's role selector uses the accessible name, including input buttons.
            let confirm = page
                .locator("role=button[name=/^(确认付款|确认支付|立即付款)$/]")
                .or_(&page.locator("role=link[name=/^(确认付款|确认支付|立即付款)$/]"));
            if confirm.count().await? == 1
                && confirm.is_visible().await?
                && confirm.is_enabled().await?
            {
                commit_clicked = true;
                state.lock().unwrap_or_else(|e| e.into_inner()).commit_armed = true;
                confirm.click(None).await?;
            }
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}
async fn fill_password(page: &Page, password: &str) -> playwright_rs::Result<bool> {
    let fields = page.locator("input[type=password]:visible");
    if fields.count().await? != 1 {
        return Ok(false);
    }
    fields.fill(password, None).await?;
    Ok(true)
}
async fn visible_text(page: &Page, texts: &[&str]) -> playwright_rs::Result<bool> {
    for text in texts {
        if page.get_by_text(text, false).first().is_visible().await? {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(test)]
mod browser_tests;
