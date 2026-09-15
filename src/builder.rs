use crate::{AttemptStore, BrowserPayer, Client, CookieJar, FileAttemptStore, HttpTransport};
use anyhow::{Result, ensure};
use std::{path::PathBuf, sync::Arc};

/// Ready-to-use client returned by `Client::builder()`.
pub type PaymentClient = Client<HttpTransport, Option<BrowserPayer>>;

pub struct ClientBuilder {
    jar: Arc<CookieJar>,
    proxy: Option<String>,
    user_agent: String,
    sso_origins: Vec<String>,
    cookies: Vec<(String, String)>,
    browser: Option<BrowserPayer>,
    state_directory: PathBuf,
    store: Option<Arc<dyn AttemptStore>>,
}
impl Default for ClientBuilder {
    fn default() -> Self {
        Self {
            jar: Arc::new(CookieJar::default()),
            proxy: None,
            user_agent: format!("cc-pay/{}", env!("CARGO_PKG_VERSION")),
            sso_origins: Vec::new(),
            cookies: Vec::new(),
            browser: None,
            state_directory: ".cc-pay/attempts".into(),
            store: None,
        }
    }
}
impl Client {
    pub fn builder() -> ClientBuilder {
        ClientBuilder::default()
    }
}
impl ClientBuilder {
    pub fn cookie_jar(mut self, jar: Arc<CookieJar>) -> Self {
        self.jar = jar;
        self
    }
    /// A Cookie request header (`name=value; other=value`), not a Set-Cookie header.
    /// The origin must be a payment origin or one explicitly added with sso_origin.
    pub fn cookie_header(mut self, origin: impl Into<String>, header: impl Into<String>) -> Self {
        self.cookies.push((origin.into(), header.into()));
        self
    }
    pub fn proxy(mut self, proxy: impl Into<String>) -> Self {
        self.proxy = Some(proxy.into());
        self
    }
    pub fn user_agent(mut self, user_agent: impl Into<String>) -> Self {
        self.user_agent = user_agent.into();
        self
    }
    pub fn sso_origin(mut self, origin: impl Into<String>) -> Self {
        self.sso_origins.push(origin.into());
        self
    }
    /// Enable the optional official Alipay browser component. The builder uses
    /// the same proxy for protocol requests and the browser, overriding payer.proxy.
    pub fn alipay_browser(mut self, payer: BrowserPayer) -> Self {
        self.browser = Some(payer);
        self
    }
    pub fn state_directory(mut self, directory: impl Into<PathBuf>) -> Self {
        self.state_directory = directory.into();
        self
    }
    pub fn attempt_store(mut self, store: Arc<dyn AttemptStore>) -> Self {
        self.store = Some(store);
        self
    }
    pub fn build(mut self) -> Result<PaymentClient> {
        let transport = HttpTransport::new(
            self.jar.clone(),
            self.proxy.as_deref(),
            &self.user_agent,
            &self
                .sso_origins
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
        )?;
        for (origin, header) in self.cookies {
            let origin = transport.validate(&origin)?;
            ensure!(
                origin.path() == "/" && origin.query().is_none() && origin.fragment().is_none(),
                "Cookie 地址必须是 origin"
            );
            ensure!(
                header.split(';').any(|part| !part.trim().is_empty())
                    && header.len() <= 16384
                    && !header.contains(['\r', '\n']),
                "Cookie 请求头无效"
            );
            for cookie in header.split(';').map(str::trim).filter(|s| !s.is_empty()) {
                let (name, value) = cookie
                    .split_once('=')
                    .ok_or_else(|| anyhow::anyhow!("Cookie 请求头无效"))?;
                ensure!(
                    !name.is_empty()
                        && name
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
                        && value
                            .bytes()
                            .all(|b| (0x21..=0x7e).contains(&b) && !b"\";,\\".contains(&b)),
                    "Cookie 请求头无效"
                );
                self.jar
                    .add_cookie_str(&format!("{name}={value}; Path=/; Secure"), &origin);
            }
        }
        if let Some(browser) = self.browser.as_mut() {
            browser.proxy = self.proxy.clone();
        }
        let store = match self.store {
            Some(store) => store,
            None => Arc::new(FileAttemptStore::new(self.state_directory)?),
        };
        Ok(Client::with_password_payer(transport, self.browser).with_attempt_store(store))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::cookie::CookieStore;
    #[test]
    fn cookie_headers_preserve_all_values_and_browser_shares_protocol_proxy() {
        let jar = Arc::new(CookieJar::default());
        let dir = tempfile::tempdir().unwrap();
        let client = Client::builder()
            .cookie_jar(jar.clone())
            .cookie_header("https://cashier.cc-pay.cn", "first=one; second=two==")
            .proxy("socks5://user:pass@10.0.0.2:1080")
            .alipay_browser(BrowserPayer {
                proxy: Some("http://wrong.invalid:80".into()),
                ..Default::default()
            })
            .state_directory(dir.path())
            .build()
            .unwrap();
        let cookies = jar
            .cookies(&url::Url::parse("https://cashier.cc-pay.cn/transaction").unwrap())
            .unwrap();
        let cookies = cookies.to_str().unwrap();
        assert!(cookies.contains("first=one") && cookies.contains("second=two=="));
        assert_eq!(
            client.password_payer.unwrap().proxy.as_deref(),
            Some("socks5://user:pass@10.0.0.2:1080")
        );
    }
    #[test]
    fn invalid_cookie_and_unapproved_origins_fail_before_state_creation() {
        for (origin, value) in [
            ("https://untrusted.example.org", "id=one"),
            ("https://cashier.cc-pay.cn", "id=x\r\nOther: y"),
            ("https://cashier.cc-pay.cn", "not-a-cookie"),
        ] {
            assert!(
                Client::builder()
                    .cookie_header(origin, value)
                    .build()
                    .is_err()
            );
        }
    }
}
