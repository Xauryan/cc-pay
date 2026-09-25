use cc_pay::{AlipayCredentials, Fields, PasswordPayment, PasswordReason};
use serde_json::Value;
use url::Url;

pub(crate) const AUTH_HOST: &str = "excashier.alipay.com";
pub(crate) const AUTH_PATH: &str = "/standard/securityPost.json";
const COMMIT_PATH: &str = "/business/api/paycommit.json";
pub(crate) const RETURN_HOST: &str = "cashier.cc-pay.cn";
const POST_PATHS: &[&str] = &[
    "/standard/switchToStdFront.htm",
    "/standard/securityRender.json",
    "/business/api/acceptPay.json",
    "/business/api/cashiermain.json",
    "/business/api/switchchannelsel.json",
    "/business/api/error.json",
];

pub(crate) fn pay_host(host: &str) -> bool {
    matches!(host, "cashier.alipay.com" | AUTH_HOST)
}
fn allowed_url(raw: &str) -> Option<Url> {
    let u = Url::parse(raw).ok()?;
    let host = u.host_str()?;
    (u.scheme() == "https"
        && u.username().is_empty()
        && u.password().is_none()
        && u.port_or_known_default() == Some(443)
        && (host == RETURN_HOST
            || ["alipay.com", "alipayobjects.com", "alipaylog.com"]
                .iter()
                .any(|domain| host == *domain || host.ends_with(&format!(".{domain}")))))
    .then_some(u)
}
fn money(value: &str) -> Option<u64> {
    let (whole, fraction) = value.split_once('.').unwrap_or((value, ""));
    if whole.is_empty()
        || whole.len() > 8
        || (whole.len() > 1 && whole.starts_with('0'))
        || !whole.bytes().all(|v| v.is_ascii_digit())
        || fraction.len() > 2
        || !fraction.bytes().all(|v| v.is_ascii_digit())
        || value.ends_with('.')
    {
        return None;
    }
    Some(
        whole.parse::<u64>().ok()? * 100
            + match fraction.len() {
                0 => 0,
                1 => fraction.parse::<u64>().ok()? * 10,
                _ => fraction.parse().ok()?,
            },
    )
}
fn unique_fields(raw: &str) -> Option<Fields> {
    let mut fields = Fields::new();
    for (key, value) in url::form_urlencoded::parse(raw.as_bytes()) {
        if fields
            .insert(key.into_owned(), value.into_owned())
            .is_some()
        {
            return None;
        }
    }
    Some(fields)
}

pub(crate) fn valid_input(
    action: &str,
    fields: &Fields,
    amount: &str,
    credentials: AlipayCredentials<'_>,
) -> bool {
    let validate = || -> Option<()> {
        if !(3..=254).contains(&credentials.account.len())
            || credentials.account.contains(['\r', '\n'])
            || credentials.password.len() != 6
            || !credentials.password.bytes().all(|b| b.is_ascii_digit())
            || action.len() > 32768
            || fields.len() > 50
            || fields
                .iter()
                .any(|(k, v)| k.len() > 1024 || v.len() > 65536)
            || fields.iter().map(|(k, v)| k.len() + v.len()).sum::<usize>() > 262144
        {
            return None;
        }
        let u = allowed_url(action)?;
        if u.host_str()? != "openapi.alipay.com"
            || u.path() != "/gateway.do"
            || u.fragment().is_some()
        {
            return None;
        }
        let mut params = unique_fields(u.query().unwrap_or(""))?;
        for (key, value) in fields {
            if params.insert(key.clone(), value.clone()).is_some() {
                return None;
            }
        }
        if params.get("sign")?.is_empty() || params.get("method")? != "alipay.trade.page.pay" {
            return None;
        }
        let biz: Value = serde_json::from_str(params.get("biz_content")?).ok()?;
        let total = match &biz["total_amount"] {
            Value::String(v) => v.clone(),
            Value::Number(v) => v.to_string(),
            _ => return None,
        };
        if biz["out_trade_no"].as_str()?.trim().is_empty()
            || biz["product_code"] != "FAST_INSTANT_TRADE_PAY"
            || biz.get("trans_currency").is_some_and(|v| v != "CNY")
            || money(&total)? == 0
            || money(&total)? != money(amount)?
        {
            return None;
        }
        Some(())
    };
    validate().is_some()
}

#[derive(Default, Debug)]
pub(crate) struct Gate {
    pub closed: bool,
    gateway_used: bool,
    pub auth_armed: bool,
    pub commit_armed: bool,
    pub password_submitted: bool,
    pub debit_submitted: bool,
    pub rejected: bool,
    pub invalid_component: bool,
    pub unsupported: bool,
}
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Decision {
    Abort,
    Gateway,
    Continue,
}
impl Gate {
    pub fn result(&self, reason: PasswordReason) -> PasswordPayment {
        if self.debit_submitted || reason == PasswordReason::ConfirmationRequired {
            PasswordPayment::uncertain(self.password_submitted, self.debit_submitted)
        } else if self.rejected {
            PasswordPayment::rejected(self.password_submitted)
        } else {
            PasswordPayment::not_submitted(reason).with_password_submitted(self.password_submitted)
        }
    }
    /// Called under a short mutex, before any network await. Claims are atomic.
    pub fn decide(
        &mut self,
        raw: &str,
        method: &str,
        navigation: bool,
        body: &str,
        action: &str,
        account: &str,
    ) -> Decision {
        if self.closed {
            return Decision::Abort;
        }
        let Some(url) = allowed_url(raw) else {
            return Decision::Abort;
        };
        let host = url.host_str().unwrap_or("");
        if raw == action && navigation && !self.gateway_used {
            self.gateway_used = true;
            return Decision::Gateway;
        }
        if host == "openapi.alipay.com" {
            return Decision::Abort;
        }
        if url.path() == COMMIT_PATH {
            if !pay_host(host) || method != "POST" || !self.commit_armed || self.debit_submitted {
                return Decision::Abort;
            }
            self.debit_submitted = true;
        } else if url.path() == AUTH_PATH {
            if host != AUTH_HOST || method != "POST" || !self.auth_armed || self.password_submitted
            {
                return Decision::Abort;
            }
            let valid = unique_fields(body).is_some_and(|fields| {
                let encrypted = fields.get("password").map(String::as_str).unwrap_or("");
                fields.get("loginId").is_some_and(|v| v == account)
                    && encrypted.len() == 344
                    && encrypted.ends_with("==")
                    && encrypted.as_bytes()[..342]
                        .iter()
                        .copied()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/')
                    && ["rdsUa", "securityId"]
                        .iter()
                        .all(|key| fields.get(*key).is_some_and(|v| !v.is_empty()))
            });
            if !valid {
                self.invalid_component = true;
                return Decision::Abort;
            }
            self.password_submitted = true;
        } else if method == "POST" {
            if !pay_host(host) || !POST_PATHS.contains(&url.path()) {
                self.unsupported = true;
                return Decision::Abort;
            }
        } else if method != "GET" && method != "HEAD" {
            return Decision::Abort;
        }
        if host == RETURN_HOST && method != "GET" {
            return Decision::Abort;
        }
        Decision::Continue
    }
}

pub(crate) fn provider_error(value: &Value) -> bool {
    value["errorCode"].as_str().is_some_and(|v| !v.is_empty())
        || value["errorCode"]
            .as_array()
            .is_some_and(|v| v.iter().any(|v| v.as_str().is_some_and(|s| !s.is_empty())))
        || value["redirectUrl"]
            .as_str()
            .and_then(|v| Url::parse(v).ok())
            .is_some_and(|u| {
                u.query_pairs()
                    .any(|(k, v)| k == "errorCode" && !v.is_empty())
            })
}

#[cfg(test)]
mod tests {
    use super::*;
    use cc_pay::PasswordOutcome;
    #[test]
    fn commit_is_once_and_timeout_or_rejection_cannot_release_it() {
        let mut g = Gate::default();
        let url = format!("https://{AUTH_HOST}{COMMIT_PATH}");
        assert_eq!(g.decide(&url, "POST", false, "", "", ""), Decision::Abort);
        g.commit_armed = true;
        assert_eq!(
            g.decide(&url, "POST", false, "", "", ""),
            Decision::Continue
        );
        assert_eq!(g.decide(&url, "POST", false, "", "", ""), Decision::Abort);
        g.rejected = true;
        assert_eq!(
            g.result(PasswordReason::Timeout).outcome(),
            PasswordOutcome::Uncertain
        );
    }
    #[test]
    fn destinations_and_duplicate_password_fields_are_denied() {
        for url in [
            "http://excashier.alipay.com/x",
            "https://alipay.com.evil.test/x",
            "https://excashier.alipay.com:444/x",
            "https://u:p@excashier.alipay.com/x",
        ] {
            assert!(allowed_url(url).is_none());
        }
        let mut g = Gate {
            auth_armed: true,
            ..Default::default()
        };
        assert_eq!(
            g.decide(
                &format!("https://{AUTH_HOST}{AUTH_PATH}"),
                "POST",
                false,
                "loginId=a&loginId=b",
                "",
                "a"
            ),
            Decision::Abort
        );
        assert!(g.invalid_component);
        g.closed = true;
        assert_eq!(
            g.decide("https://excashier.alipay.com/x", "GET", true, "", "", ""),
            Decision::Abort
        );
    }
    #[test]
    fn signed_order_amount_and_duplicate_query_parameters_are_checked() {
        let credentials = AlipayCredentials {
            account: "demo",
            password: "123456",
        };
        let fields = Fields::from([
            ("sign".into(), "fixture".into()), ("method".into(),"alipay.trade.page.pay".into()),
            ("biz_content".into(),r#"{"out_trade_no":"T","product_code":"FAST_INSTANT_TRADE_PAY","total_amount":"1.23"}"#.into())]);
        let action = "https://openapi.alipay.com/gateway.do";
        assert!(valid_input(action, &fields, "1.23", credentials));
        assert!(!valid_input(action, &fields, "1.24", credentials));
        assert!(!valid_input(
            &format!("{action}?sign=other"),
            &fields,
            "1.23",
            credentials
        ));
        assert!(!valid_input(
            &format!("{action}?x=1&x=2"),
            &fields,
            "1.23",
            credentials
        ));
    }
}
