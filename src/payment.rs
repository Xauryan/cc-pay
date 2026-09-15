use crate::{model::*, transport::*, *};
use anyhow::{Context, Result, bail, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use regex::Regex;
use reqwest::Method;
use scraper::Html;
use serde_json::Value;
#[cfg(test)]
use serde_json::json;
use url::Url;
#[path = "payment_ecny.rs"]
mod ecny;
pub use ecny::valid_wallet_preference;

pub fn parse_post_form(html: &str) -> Result<(String, Fields)> {
    form(html, None)
}

const CASHIER: &str = "https://cashier.cc-pay.cn";
fn data(v: Value) -> Result<Value> {
    ensure!(
        v["success"] == true,
        "校园付拒绝请求：{}",
        val(&v, "message")
    );
    v.get("data").cloned().context("校园付响应缺少 data")
}
fn scalar(v: &Value, key: &str) -> Result<String> {
    match v.get(key) {
        Some(Value::String(s)) if !s.is_empty() => Ok(s.clone()),
        Some(Value::Number(n)) => Ok(n.to_string()),
        _ => bail!("校园付响应缺少有效的 {key}"),
    }
}
pub fn png(payload: &str) -> Result<String> {
    let code = qrcode::QrCode::new(payload.as_bytes())?;
    let image = code
        .render::<image::Luma<u8>>()
        .min_dimensions(320, 320)
        .build();
    let mut output = std::io::Cursor::new(vec![]);
    image.write_to(&mut output, image::ImageFormat::Png)?;
    Ok(STANDARD.encode(output.into_inner()))
}
fn decode_attribute(raw: &str) -> String {
    Regex::new(r"&(?:#[xX][0-9a-fA-F]+|#\d+|[A-Za-z][A-Za-z0-9]+);")
        .unwrap()
        .replace_all(raw, |c: &regex::Captures| {
            html_escape::decode_html_entities(&c[0]).into_owned()
        })
        .into_owned()
}
fn form(html: &str, id: Option<&str>) -> Result<(String, Fields)> {
    let document = Html::parse_document(html);
    let sel = id
        .map(|x| format!("form#{x}"))
        .unwrap_or("form[action]".into());
    let element = document
        .select(&selector(&sel))
        .next()
        .context("支付页未返回所需表单")?;
    let fragment = element.html();
    let raw = Regex::new(r#"(?is)<form\b[^>]*\baction\s*=\s*["']([^"']+)["']"#)?
        .captures(if id.is_none() { html } else { &fragment })
        .context("支付表单缺少 action")?[1]
        .to_owned();
    ensure!(
        element
            .value()
            .attr("method")
            .unwrap_or("get")
            .eq_ignore_ascii_case("post"),
        "支付表单方法变化"
    );
    Ok((decode_attribute(&raw), input_fields(&fragment)))
}

impl<T: Transport, P: PasswordPayer> Client<T, P> {
    pub async fn authenticate(&self, cashier: &str, sso_login: &str) -> Result<()> {
        ensure!(
            valid_url(cashier)?.host_str() == Some("cashier.cc-pay.cn"),
            "收银台地址无效"
        );
        self.follow(cashier).await?;
        let mut next = with_query(
            sso_login,
            &fields(&[("service", "https://pass.cc-pay.cn/login")]),
        )?;
        for _ in 0..10 {
            let r = self.get(&next).await?;
            if (300..400).contains(&r.status) {
                let u = Url::parse(&r.url)?.join(r.location.as_deref().context("认证缺少跳转")?)?;
                if u.host_str() == Some("mall.cc-pay.cn")
                    && Url::parse(&r.url)?.host_str() == Some("pass.cc-pay.cn")
                {
                    break;
                }
                next = u.into();
            } else {
                ensure!(
                    Url::parse(&r.url)?.host_str() == Some("cashier.cc-pay.cn"),
                    "校园付需要交互登录"
                );
                break;
            }
        }
        let r = self.follow(cashier).await?;
        ensure!(
            Url::parse(&r.url)?.host_str() == Some("cashier.cc-pay.cn"),
            "校园付登录未完成"
        );
        Ok(())
    }
    pub(crate) async fn cashier_get(&self, path: &str, args: &Fields) -> Result<Value> {
        data(self.cashier_response(path, args).await?)
    }
    async fn cashier_response(&self, path: &str, args: &Fields) -> Result<Value> {
        let mut q = args.clone();
        q.insert(
            "_t".into(),
            chrono::Utc::now().timestamp_millis().to_string(),
        );
        self.get(&with_query(&format!("{CASHIER}{path}"), &q)?)
            .await?
            .json()
    }

    pub async fn transaction(&self, cashier: &str) -> Result<Value> {
        self.cashier_get("/transaction", &fields(&[("id", &cashier_id(cashier)?)]))
            .await
    }
    async fn start_payment(&self, cashier: &str, method: &str) -> Result<(Value, Value)> {
        let channel = match method {
            "wechat" => "wxpay_web",
            "alipay" => "alipay_web",
            _ => bail!("支付方式无效"),
        };
        let tx = self.transaction(cashier).await?;
        ensure!(
            val(&tx, "status") == "wait_payer_pay",
            "订单不再待支付：{}",
            val(&tx, "status")
        );
        let ways = self
            .cashier_get(
                "/api/pay_ways",
                &fields(&[("goodsId", &scalar(&tx, "goodsId")?)]),
            )
            .await?;
        let matches = ways["normal"]
            .as_array()
            .context("支付渠道格式无效")?
            .iter()
            .filter(|v| {
                val(v, "name") == channel && v["isActive"] == true && v["isDeleted"] != true
            })
            .collect::<Vec<_>>();
        ensure!(matches.len() == 1, "没有唯一可用支付渠道：{channel}");
        let v = self
            .cashier_get(
                "/transaction/pay",
                &fields(&[
                    ("id", &cashier_id(cashier)?),
                    ("payWayId", &scalar(matches[0], "id")?),
                ]),
            )
            .await?;
        ensure!(v["isPaid"] != true, "订单已支付");
        ensure!(
            v.get("transactionId").is_none()
                || scalar(&v, "transactionId")? == cashier_id(cashier)?,
            "支付订单编号不匹配"
        );
        Ok((v, tx))
    }
    async fn alipay_page(&self, payload: &Value) -> Result<Response> {
        let (action, values) = form(val(payload, "payWebForm"), None)?;
        let u = valid_url(&action)?;
        ensure!(
            u.host_str() == Some("openapi.alipay.com")
                && u.path() == "/gateway.do"
                && values.contains_key("biz_content"),
            "支付宝签名表单无效"
        );
        let r = self
            .request(Method::POST, &action, Some(&values), Some(CASHIER))
            .await?;
        let target =
            Url::parse(&r.url)?.join(r.location.as_deref().context("支付宝网关未返回跳转")?)?;
        self.follow(target.as_str()).await
    }
    pub async fn create_payment(
        &self,
        cashier: &str,
        method: PaymentMethod,
        options: PaymentOptions<'_>,
    ) -> Result<Payment> {
        let method = method.as_str();
        let id = cashier_id(cashier)?;
        ensure!(
            !self.attempts.contains(&id)?,
            "该订单已有自动付款尝试，请仅查询付款状态"
        );
        if method == "ecny" {
            return self
                .ecny_payment(cashier, options.ecny_wallet_index, options.expected_amount)
                .await;
        }
        let automatic = method == "alipay" && options.alipay.is_some();
        let (mut payload, tx) = match self.start_payment(cashier, method).await {
            Ok(result) => result,
            Err(_) if automatic => return Ok(unsubmitted_payment(cashier, method)),
            Err(error) => return Err(error),
        };
        let amount = match scalar(&tx, "money") {
            Ok(amount) => amount,
            Err(_) if automatic => return Ok(unsubmitted_payment(cashier, method)),
            Err(error) => return Err(error),
        };
        if let Some(expected) = options.expected_amount {
            ensure!(
                ecny::cents(expected).is_some_and(|v| v > 0)
                    && ecny::cents(expected) == ecny::cents(&amount),
                "校园付金额与预期不符，未提交自动扣款"
            );
        }
        let mut payment = Payment {
            method: method.into(),
            status: val(&tx, "status").into(),
            amount,
            cashier_url: cashier.into(),
            qr_png: None,
            created_at: val(&tx, "tsCreation").into(),
            expires_policy: val(&tx, "expireDuration").into(),
            account_attempt: None,
            password_submitted: false,
            automatic: None,
        };
        if automatic {
            let (action, values) = match form(val(&payload, "payWebForm"), None) {
                Ok(form) => form,
                Err(_) => return Ok(unsubmitted_payment(cashier, method)),
            };
            self.claim_attempt(cashier)?;
            let result = self
                .password_payer
                .pay(&action, &values, &payment.amount, options.alipay.unwrap())
                .await;
            payment.account_attempt = Some(result.message().into());
            payment.password_submitted = result.password_submitted;
            payment.automatic = Some(AutomaticPayment {
                outcome: result.outcome,
                debit_submitted: result.debit_submitted,
            });
            // Campus transaction state is authoritative; allow its callback time to arrive.
            // A stale wait_payer_pay after a submitted debit is not proof of failure.
            for check in 0..3 {
                if check > 0 {
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                }
                match self.transaction(cashier).await {
                    Ok(tx) if !val(&tx, "status").is_empty() => {
                        apply_transaction(&mut payment, &tx);
                        if payment.status != "wait_payer_pay" || !payment.needs_confirmation() {
                            break;
                        }
                    }
                    _ => {
                        payment.account_attempt =
                            Some("自动付款已结束，但校园付状态查询失败，请先查询付款状态".into());
                        return Ok(payment);
                    }
                }
            }
            if payment.status != "wait_payer_pay" || payment.needs_confirmation() {
                return Ok(payment);
            }
        }
        let qr = async {
            if method == "wechat" {
                let qr = val(&payload, "payQrCode");
                ensure!(
                    Regex::new(r"^weixin://wxpay/bizpayurl\?pr=[A-Za-z0-9_-]+$")?.is_match(qr),
                    "未返回有效微信二维码"
                );
                return png(qr);
            }
            if automatic {
                payload = self.start_payment(cashier, method).await?.0;
            }
            let page = self.alipay_page(&payload).await?;
            let qr = input(&page.body, "J_qrCode").context("支付宝未返回二维码")?;
            let u = Url::parse(&qr)?;
            ensure!(
                u.scheme() == "https"
                    && u.host_str() == Some("qr.alipay.com")
                    && u.username().is_empty()
                    && u.password().is_none(),
                "支付宝二维码地址无效"
            );
            png(&qr)
        }
        .await;
        match qr {
            Ok(image) => payment.qr_png = Some(image),
            Err(_) if automatic => {
                payment.account_attempt = Some(format!(
                    "{}；扫码入口获取失败，请查询订单状态或到校园付收银台处理",
                    payment
                        .account_attempt
                        .as_deref()
                        .unwrap_or("自动付款未完成")
                ));
                return Ok(payment);
            }
            Err(error) => return Err(error),
        }
        let tx = match self.transaction(cashier).await {
            Ok(tx) if !val(&tx, "status").is_empty() => tx,
            _ if automatic => {
                payment.qr_png = None;
                payment.account_attempt = Some(
                    "自动付款未完成，校园付状态查询失败；请查询状态或到校园付收银台处理".into(),
                );
                return Ok(payment);
            }
            _ => bail!("校园付未返回可识别的订单状态"),
        };
        apply_transaction(&mut payment, &tx);
        Ok(payment)
    }
}
fn unsubmitted_payment(cashier: &str, method: &str) -> Payment {
    Payment {
        method: method.into(),
        status: "preparing".into(),
        amount: String::new(),
        cashier_url: cashier.into(),
        qr_png: None,
        created_at: now(),
        expires_policy: String::new(),
        account_attempt: Some(
            "自动付款尚未提交，支付入口暂不可用；请查询状态或到校园付收银台处理".into(),
        ),
        password_submitted: false,
        automatic: Some(AutomaticPayment {
            outcome: "not_submitted".into(),
            debit_submitted: false,
        }),
    }
}

fn apply_transaction(payment: &mut Payment, tx: &Value) {
    payment.status = val(tx, "status").into();
    if payment.status != "wait_payer_pay" {
        payment.qr_png = None;
        if let Some(attempt) = payment.automatic.as_mut() {
            // The recorded campus cashier recognizes `success` as paid. Other
            // unfamiliar states must not release an uncertain debit for retry.
            if payment.status == "success" {
                attempt.outcome = "verified".into();
            }
            payment.account_attempt = Some(format!(
                "自动付款尝试结束，校园付返回订单状态：{}",
                payment.status
            ));
        }
    }
}

pub fn cashier_id(cashier: &str) -> Result<String> {
    let u = valid_url(cashier)?;
    ensure!(u.host_str() == Some("cashier.cc-pay.cn"), "收银台域名无效");
    let ids = u
        .query_pairs()
        .filter(|(k, _)| k == "id")
        .map(|(_, v)| v.into_owned())
        .collect::<Vec<_>>();
    ensure!(
        ids.len() == 1 && !ids[0].is_empty(),
        "收银台缺少唯一订单编号"
    );
    Ok(ids[0].clone())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unfamiliar_transaction_states_keep_submitted_payments_protected() {
        let mut payment: Payment =
            serde_json::from_value(json!({"method":"alipay","status":"wait_payer_pay",
            "amount":"1.00","cashier_url":"","qr_png":null,"created_at":"","expires_policy":"",
            "account_attempt":null,"password_submitted":true,
            "automatic":{"outcome":"uncertain","debit_submitted":true}}))
            .unwrap();
        apply_transaction(&mut payment, &json!({"status":"processing"}));
        assert!(payment.needs_confirmation());
        assert!(payment.qr_png.is_none());
        apply_transaction(&mut payment, &json!({"status":"success"}));
        assert!(!payment.needs_confirmation());
        assert!(payment.qr_png.is_none());
    }
    #[test]
    fn signed_action_preserves_parameters() {
        let s = r#"<form method="post" action="https://openapi.alipay.com/gateway.do?sign=S&notify_url=X&timestamp=now"><input name="biz_content" value="{}"></form>"#;
        let (a, _) = form(s, None).unwrap();
        assert!(a.contains("&notify_url=X&timestamp=now"));
        assert_eq!(
            decode_attribute("a=1&amp;b=2&timestamp=3"),
            "a=1&b=2&timestamp=3"
        );
    }
    #[test]
    fn qr_png_valid() {
        let b = STANDARD
            .decode(png("weixin://wxpay/bizpayurl?pr=TEST").unwrap())
            .unwrap();
        assert!(b.starts_with(b"\x89PNG\r\n\x1a\n"));
        assert!(image::load_from_memory(&b).is_ok());
    }
    #[test]
    fn invalid_cashier() {
        assert!(cashier_id("https://cashier.cc-pay.cn/cashier?id=1&id=2").is_err());
        assert!(cashier_id("https://sso.example.org/login?id=1").is_err());
    }
}
