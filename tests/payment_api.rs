//! Consumer-facing regression tests. All network responses are fixtures.
use cc_pay::{
    AlipayCredentials, Client, Fields, MemoryAttemptStore, PasswordPayer, PasswordPayment,
    PaymentMethod, PaymentOptions, Response, Transport,
};
use reqwest::Method;
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

const CASHIER: &str = "https://cashier.cc-pay.cn/cashier?id=T";
struct Step {
    path: &'static str,
    response: Option<Value>,
}
#[derive(Clone)]
struct Script(Arc<Mutex<VecDeque<Step>>>);
impl Script {
    fn new(steps: Vec<Step>) -> Self {
        Self(Arc::new(Mutex::new(steps.into())))
    }
    fn done(&self) {
        assert!(
            self.0.lock().unwrap().is_empty(),
            "expected requests were not performed"
        );
    }
}
impl Transport for Script {
    async fn request(
        &self,
        method: Method,
        url: &str,
        _: Option<&Fields>,
        _: Option<&str>,
    ) -> anyhow::Result<Response> {
        let url = url::Url::parse(url)?;
        assert_eq!(method, Method::GET);
        let step = self
            .0
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected request, possible retry/fallback");
        assert_eq!(url.path(), step.path);
        let body = step
            .response
            .ok_or_else(|| anyhow::anyhow!("simulated lost response"))?;
        Ok(Response {
            url: url.into(),
            status: 200,
            location: None,
            body: body.to_string(),
        })
    }
}
fn ok(path: &'static str, data: Value) -> Step {
    Step {
        path,
        response: Some(json!({"success":true,"data":data})),
    }
}
fn tx() -> Step {
    ok(
        "/transaction",
        json!({"id":"T","status":"wait_payer_pay","currency":"CNY","money":"1.00","goodsId":"G"}),
    )
}
fn ways(name: &str) -> Step {
    ok(
        "/api/pay_ways",
        json!({"normal":[{"name":name,"id":"P","isActive":true}],"ecCode":["icbc","ccb"]}),
    )
}
fn credentials() -> AlipayCredentials<'static> {
    AlipayCredentials {
        account: "test-account",
        password: "123456",
    }
}
struct UncertainPayer(Arc<AtomicUsize>);
impl PasswordPayer for UncertainPayer {
    async fn pay(
        &self,
        _: &str,
        _: &Fields,
        amount: &str,
        _: AlipayCredentials<'_>,
    ) -> PasswordPayment {
        assert_eq!(amount, "1.00");
        self.0.fetch_add(1, Ordering::SeqCst);
        PasswordPayment::uncertain(true, true)
    }
}

#[tokio::test]
async fn automatic_payments_without_explicit_store_make_no_requests() {
    let script = Script::new(vec![]);
    let client = Client::new(script.clone());
    for method in [PaymentMethod::Ecny, PaymentMethod::Alipay] {
        let error = client
            .create_payment(
                CASHIER,
                method,
                PaymentOptions {
                    alipay: Some(credentials()),
                    ..Default::default()
                },
            )
            .await
            .err()
            .unwrap();
        assert!(error.to_string().contains("AttemptStore"));
    }
    script.done();
}

#[tokio::test]
async fn qr_payment_needs_no_persistence_or_browser() {
    let script = Script::new(vec![
        tx(),
        ways("wxpay_web"),
        ok(
            "/transaction/pay",
            json!({"transactionId":"T","payQrCode":"weixin://wxpay/bizpayurl?pr=TEST"}),
        ),
        tx(),
    ]);
    let payment = Client::new(script.clone())
        .create_payment(CASHIER, PaymentMethod::Wechat, PaymentOptions::default())
        .await
        .unwrap();
    assert!(payment.qr_png.is_some());
    assert!(payment.automatic.is_none());
    script.done();
}

#[tokio::test(start_paused = true)]
async fn uncertain_alipay_debit_never_creates_a_second_payment_or_qr() {
    let script = Script::new(vec![
        tx(),
        ways("alipay_web"),
        ok(
            "/transaction/pay",
            json!({"transactionId":"T","payWebForm":"<form method='post' action='https://openapi.alipay.com/gateway.do'><input name='biz_content' value='{}'></form>"}),
        ),
        tx(),
        tx(),
        tx(),
    ]);
    let calls = Arc::new(AtomicUsize::new(0));
    let client = Client::with_password_payer(script.clone(), UncertainPayer(calls.clone()))
        .with_attempt_store(Arc::new(MemoryAttemptStore::default()));
    let payment = client
        .create_payment(
            CASHIER,
            PaymentMethod::Alipay,
            PaymentOptions {
                alipay: Some(credentials()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(payment.needs_confirmation());
    assert!(payment.qr_png.is_none());
    assert!(payment.password_submitted);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(
        client
            .create_payment(CASHIER, PaymentMethod::Wechat, PaymentOptions::default())
            .await
            .is_err()
    );
    script.done();
}

#[tokio::test(start_paused = true)]
async fn lost_ecny_response_stops_wallet_rotation_and_protects_order() {
    let script = Script::new(vec![
        tx(),
        ways("ecpay_token"),
        Step {
            path: "/transaction/pay",
            response: None,
        },
        tx(),
        tx(),
        tx(),
    ]);
    let client =
        Client::new(script.clone()).with_attempt_store(Arc::new(MemoryAttemptStore::default()));
    let payment = client
        .create_payment(CASHIER, PaymentMethod::Ecny, PaymentOptions::default())
        .await
        .unwrap();
    assert!(payment.needs_confirmation());
    assert!(payment.automatic.unwrap().debit_submitted);
    assert!(
        client
            .create_payment(CASHIER, PaymentMethod::Ecny, PaymentOptions::default())
            .await
            .is_err()
    );
    script.done();
}

#[tokio::test]
async fn expected_amount_mismatch_prevents_calling_the_payer() {
    let script = Script::new(vec![
        tx(),
        ways("alipay_web"),
        ok("/transaction/pay", json!({"transactionId":"T"})),
    ]);
    let calls = Arc::new(AtomicUsize::new(0));
    let client = Client::with_password_payer(script.clone(), UncertainPayer(calls.clone()))
        .with_attempt_store(Arc::new(MemoryAttemptStore::default()));
    assert!(
        client
            .create_payment(
                CASHIER,
                PaymentMethod::Alipay,
                PaymentOptions {
                    alipay: Some(credentials()),
                    expected_amount: Some("1.01"),
                    ..Default::default()
                }
            )
            .await
            .is_err()
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    script.done();
}
