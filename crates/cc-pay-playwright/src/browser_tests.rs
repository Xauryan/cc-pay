//! Explicit opt-in local browser fixtures. Every URL is fulfilled or aborted.
use super::*;
use cc_pay::PasswordOutcome;
use playwright_rs::protocol::route::FulfillOptions;
use playwright_rs::{LaunchOptions, Playwright};

async fn browser() -> playwright_rs::Result<(Playwright, Browser)> {
    let playwright = Playwright::launch().await?;
    let mut options = LaunchOptions::default().headless(true).args(vec![
        "--disable-quic".into(),
        "--force-webrtc-ip-handling-policy=disable_non_proxied_udp".into(),
        // Hard stop for unmocked destinations; local tests never reach a provider.
        "--host-resolver-rules=MAP * ~NOTFOUND".into(),
    ]);
    if let Ok(path) = std::env::var("CC_PAY_TEST_BROWSER") {
        options = options.executable_path(path);
    }
    let browser = playwright.chromium().launch_with_options(options).await?;
    Ok((playwright, browser))
}

#[tokio::test]
#[ignore = "requires a provisioned Playwright driver and Chromium; no payment network"]
async fn local_official_component_fixture_submits_once() -> playwright_rs::Result<()> {
    let (playwright, browser) = browser().await?;
    let context = browser.new_context().await?;
    let state = Arc::new(Mutex::new(Gate::default()));
    let captured = state.clone();
    context.route("**/*", move |route| {
        let state = captured.clone();
        async move {
            let request = route.request();
            let decision = state.lock().unwrap().decide(request.url(),request.method(),request.is_navigation_request(),&request.post_data().unwrap_or_default(),"", "fixture-account");
            if decision == Decision::Abort { return route.abort(None).await; }
            let path = url::Url::parse(request.url()).unwrap().path().to_owned();
            let (content_type, body) = match path.as_str() {
                "/standard/start.htm" => ("text/html; charset=utf-8", r#"
                    <form id="J_TloginForm" onsubmit="return false">
                    <input name="loginId"><input name="securityId" value="fixture">
                    <input type="password"><button id="J_newBtn" type="button">继续</button></form>
                    <script>
                    window.light={page:{products:{payPasswd:{}}}}; window.json_ua='fixture';
                    document.querySelector('#J_newBtn').onclick=async()=>{
                      const fields=new URLSearchParams({loginId:document.querySelector('[name=loginId]').value,
                        password:'A'.repeat(342)+'==',rdsUa:'fixture',securityId:'fixture'});
                      await fetch('/standard/securityPost.json',{method:'POST',body:fields});
                      location.href='/business/confirm.htm';
                    };
                    </script>"#),
                AUTH_PATH => ("application/json", "{}"),
                "/business/confirm.htm" => ("text/html; charset=utf-8", r#"
                    <input type="password"><button onclick="fetch('/business/api/paycommit.json',{method:'POST',body:'fixture'})">确认付款</button>"#),
                "/business/api/paycommit.json" => ("application/json", "{}"),
                _ => return route.abort(None).await,
            };
            route.fulfill(Some(FulfillOptions::builder().status(200).content_type(content_type).body_string(body).build())).await
        }
    }).await?;
    let page = context.new_page().await?;
    page.set_default_timeout(1500.0).await;
    page.goto("https://excashier.alipay.com/standard/start.htm", None)
        .await?;
    let reason = tokio::time::timeout(
        Duration::from_secs(15),
        interact(
            &page,
            state.clone(),
            AlipayCredentials {
                account: "fixture-account",
                password: "123456",
            },
        ),
    )
    .await
    .unwrap_or_else(|_| {
        panic!(
            "fixture timed out at {} with {:?}",
            page.url(),
            state.lock().unwrap()
        )
    })?;
    let result = state.lock().unwrap().result(reason);
    assert!(result.password_submitted());
    assert!(result.debit_submitted());
    assert_eq!(result.outcome(), PasswordOutcome::Uncertain);
    context.close().await?;
    browser.close().await?;
    playwright.shutdown().await?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires a provisioned Playwright driver and Chromium; no payment network"]
async fn cancellation_and_late_context_delivery_release_only_owned_contexts()
-> playwright_rs::Result<()> {
    let (playwright, browser) = browser().await?;
    let host_context = browser.new_context().await?;
    let host_page = host_context.new_page().await?;
    let capacity = Arc::new(Semaphore::new(1));
    let state = Arc::new(Mutex::new(Gate::default()));
    let guard = create_context(
        browser.clone(),
        state.clone(),
        capacity.clone().acquire_owned().await.unwrap(),
        tokio::time::Instant::now() + Duration::from_secs(10),
    )
    .await
    .unwrap();
    assert_eq!(browser.contexts().len(), 2);
    drop(guard);
    assert!(state.lock().unwrap().closed);
    let permit = tokio::time::timeout(Duration::from_secs(10), capacity.clone().acquire_owned())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(browser.contexts().len(), 1);
    // Force the receiver to time out while the context RPC is still pending.
    let late = create_context(
        browser.clone(),
        Arc::new(Mutex::new(Gate::default())),
        permit,
        tokio::time::Instant::now() - Duration::from_secs(1),
    )
    .await;
    assert!(late.is_none());
    let _permit = tokio::time::timeout(Duration::from_secs(10), capacity.acquire())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(browser.contexts().len(), 1);
    assert_eq!(host_page.url(), "about:blank");
    host_context.close().await?;
    browser.close().await?;
    playwright.shutdown().await?;
    Ok(())
}
