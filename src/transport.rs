use crate::{Client, PasswordPayer};
use anyhow::{Context, Result, anyhow, bail, ensure};
use reqwest::{Method, cookie::Jar};
use scraper::{Html, Selector};
use serde_json::Value;
use std::{collections::BTreeMap, future::Future, sync::Arc, time::Duration};
use url::Url;

pub type Fields = BTreeMap<String, String>;
pub struct Response {
    pub url: String,
    pub status: u16,
    pub location: Option<String>,
    pub body: String,
}
impl Response {
    pub fn json(&self) -> Result<Value> {
        serde_json::from_str(&self.body).context("校园付未返回有效 JSON")
    }
}
/// Implementations must share cookies, disable automatic retries/redirects, bound
/// response bodies, and never log secrets or replay `/transaction/pay` on failure.
pub trait Transport: Send + Sync {
    fn request(
        &self,
        method: Method,
        url: &str,
        form: Option<&Fields>,
        referer: Option<&str>,
    ) -> impl Future<Output = Result<Response>> + Send;
}

pub const HOSTS: &[&str] = &[
    "pass.cc-pay.cn",
    "cashier.cc-pay.cn",
    "openapi.alipay.com",
    "excashier.alipay.com",
    "cashier.alipay.com",
    "unitradeprod.alipay.com",
    "mobilecodec.alipay.com",
    "tfsimg.alipay.com",
];
fn https_url(url: &str) -> Result<Url> {
    let u = Url::parse(url).map_err(|_| anyhow!("支付地址格式无效"))?;
    ensure!(
        u.scheme() == "https"
            && u.username().is_empty()
            && u.password().is_none()
            && u.port_or_known_default() == Some(443),
        "支付地址必须为 HTTPS 443 且不含用户凭据"
    );
    Ok(u)
}
pub(crate) fn valid_url(url: &str) -> Result<Url> {
    let u = https_url(url)?;
    ensure!(
        HOSTS.contains(&u.host_str().unwrap_or("")),
        "支付地址不在授权域名内"
    );
    Ok(u)
}

/// Reuse the application's authenticated cookie jar. Explicit HTTP(S)/SOCKS5(H)
/// proxies support private networks; system proxy variables are never inherited.
#[derive(Clone)]
pub struct HttpTransport {
    http: reqwest::Client,
    sso_hosts: Vec<String>,
}
impl HttpTransport {
    pub fn new(
        jar: Arc<Jar>,
        proxy: Option<&str>,
        user_agent: &str,
        sso_origins: &[&str],
    ) -> Result<Self> {
        let mut sso_hosts = Vec::new();
        for origin in sso_origins {
            sso_hosts.push(
                https_url(origin)?
                    .host_str()
                    .context("SSO 域名无效")?
                    .to_owned(),
            );
        }
        ensure!(
            !user_agent.is_empty() && !user_agent.contains(['\r', '\n']),
            "UA 无效"
        );
        let mut b = reqwest::Client::builder()
            .no_proxy()
            .cookie_provider(jar)
            .user_agent(user_agent)
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(15));
        if let Some(proxy) = proxy {
            let u = Url::parse(proxy).map_err(|_| anyhow!("代理地址无效"))?;
            ensure!(
                ["http", "https", "socks5", "socks5h"].contains(&u.scheme())
                    && u.host_str().is_some(),
                "代理仅支持 HTTP(S)/SOCKS5(H)"
            );
            b = b.proxy(reqwest::Proxy::all(proxy).map_err(|_| anyhow!("代理地址无效"))?);
        }
        Ok(Self {
            http: b.build()?,
            sso_hosts,
        })
    }
    pub(crate) fn validate(&self, raw: &str) -> Result<Url> {
        let u = https_url(raw)?;
        let host = u.host_str().unwrap_or("");
        ensure!(
            HOSTS.contains(&host) || self.sso_hosts.iter().any(|s| s == host),
            "目标不在授权支付或 SSO 域名内"
        );
        Ok(u)
    }
}
impl Transport for HttpTransport {
    async fn request(
        &self,
        method: Method,
        url: &str,
        form: Option<&Fields>,
        referer: Option<&str>,
    ) -> Result<Response> {
        self.validate(url)?;
        let mut r = self
            .http
            .request(method, url)
            .header("Accept-Language", "zh-CN,zh;q=0.9");
        if let Some(referer) = referer {
            let u = self.validate(referer)?;
            r = r
                .header("Referer", referer)
                .header("Origin", u.origin().ascii_serialization());
        }
        if let Some(form) = form {
            r = r.form(form);
        }
        let mut r = r
            .send()
            .await
            .map_err(|_| anyhow!("支付网络请求失败，未自动重试"))?;
        let status = r.status().as_u16();
        ensure!(status < 400, "支付上游 HTTP {status}，未自动重试");
        let location = r
            .headers()
            .get("location")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let url = r.url().to_string();
        let content_type = r
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_owned();
        let mut bytes = Vec::new();
        while let Some(chunk) = r.chunk().await.map_err(|_| anyhow!("支付响应读取失败"))? {
            ensure!(bytes.len() + chunk.len() <= 8 * 1024 * 1024, "支付响应过大");
            bytes.extend_from_slice(&chunk);
        }
        let charset = regex::Regex::new(r#"(?i)charset\s*=\s*[\"']?([a-z0-9_-]+)"#).unwrap();
        let prefix = String::from_utf8_lossy(&bytes[..bytes.len().min(4096)]);
        let capture = charset
            .captures(&content_type)
            .or_else(|| charset.captures(&prefix));
        let encoding = capture
            .as_ref()
            .and_then(|c| encoding_rs::Encoding::for_label(c[1].as_bytes()))
            .unwrap_or(encoding_rs::UTF_8);
        Ok(Response {
            url,
            status,
            location,
            body: encoding.decode(&bytes).0.into_owned(),
        })
    }
}
impl<T: Transport, P: PasswordPayer> Client<T, P> {
    pub(crate) async fn request(
        &self,
        method: Method,
        url: &str,
        form: Option<&Fields>,
        referer: Option<&str>,
    ) -> Result<Response> {
        self.transport.request(method, url, form, referer).await
    }
    pub(crate) async fn get(&self, url: &str) -> Result<Response> {
        self.request(Method::GET, url, None, None).await
    }
    pub(crate) async fn follow(&self, url: &str) -> Result<Response> {
        let mut next = url.to_owned();
        for _ in 0..12 {
            let r = self.get(&next).await?;
            if (300..400).contains(&r.status) {
                next = Url::parse(&r.url)?
                    .join(r.location.as_deref().context("支付跳转缺少 Location")?)?
                    .into();
            } else {
                return Ok(r);
            }
        }
        bail!("支付跳转次数过多")
    }
}
pub(crate) fn fields(pairs: &[(&str, &str)]) -> Fields {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}
pub(crate) fn with_query(base: &str, args: &Fields) -> Result<String> {
    let mut u = Url::parse(base)?;
    u.query_pairs_mut().extend_pairs(args);
    Ok(u.into())
}
pub(crate) fn val<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}
pub(crate) fn selector(s: &str) -> Selector {
    Selector::parse(s).unwrap()
}
pub(crate) fn input(html: &str, key: &str) -> Option<String> {
    Html::parse_document(html)
        .select(&selector("input"))
        .find(|e| e.value().attr("id") == Some(key) || e.value().attr("name") == Some(key))
        .map(|e| e.value().attr("value").unwrap_or("").into())
}
pub(crate) fn input_fields(html: &str) -> Fields {
    Html::parse_fragment(html)
        .select(&selector("input[name]"))
        .filter(|e| e.value().attr("disabled").is_none())
        .map(|e| {
            (
                e.value().attr("name").unwrap().into(),
                e.value().attr("value").unwrap_or("").into(),
            )
        })
        .collect()
}
