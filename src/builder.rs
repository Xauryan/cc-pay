use crate::{
    AttemptStore, Client, CookieJar, FileAttemptStore, HttpTransport, NoPasswordPayer,
    PasswordPayer,
};
use anyhow::{Result, ensure};
use std::{path::PathBuf, sync::Arc};

/// Ready-to-use client returned by `Client::builder()`.
pub type PaymentClient<P = NoPasswordPayer> = Client<HttpTransport, P>;

pub struct ClientBuilder<P = NoPasswordPayer> {
    jar: Arc<CookieJar>,
    proxy: Option<String>,
    user_agent: String,
    sso_origins: Vec<String>,
    cookies: Vec<(String, String)>,
    payer: P,
    state_directory: Option<PathBuf>,
    store: Option<Arc<dyn AttemptStore>>,
}
impl Default for ClientBuilder<NoPasswordPayer> {
    fn default() -> Self {
        Self {
            jar: Arc::new(CookieJar::default()),
            proxy: None,
            user_agent: format!("cc-pay/{}", env!("CARGO_PKG_VERSION")),
            sso_origins: Vec::new(),
            cookies: Vec::new(),
            payer: NoPasswordPayer,
            state_directory: None,
            store: None,
        }
    }
}
impl Client {
    pub fn builder() -> ClientBuilder {
        ClientBuilder::default()
    }
}
impl<P: PasswordPayer> ClientBuilder<P> {
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
    /// Inject an application-owned payer. Its runtime and proxy configuration are
    /// independent of this HTTP client's configuration.
    pub fn password_payer<Q: PasswordPayer>(self, payer: Q) -> ClientBuilder<Q> {
        ClientBuilder {
            jar: self.jar,
            proxy: self.proxy,
            user_agent: self.user_agent,
            sso_origins: self.sso_origins,
            cookies: self.cookies,
            payer,
            state_directory: self.state_directory,
            store: self.store,
        }
    }
    /// Store durable payment claims in the supplied application-owned directory.
    pub fn state_directory(mut self, directory: impl Into<PathBuf>) -> Self {
        self.state_directory = Some(directory.into());
        self.store = None;
        self
    }
    pub fn attempt_store(mut self, store: Arc<dyn AttemptStore>) -> Self {
        self.store = Some(store);
        self.state_directory = None;
        self
    }
    pub fn build(self) -> Result<PaymentClient<P>> {
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
        let mut validated_cookies = Vec::new();
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
                validated_cookies.push((origin.clone(), format!("{name}={value}; Path=/; Secure")));
            }
        }
        let store = match (self.store, self.state_directory) {
            (Some(store), _) => Some(store),
            (_, Some(directory)) => {
                Some(Arc::new(FileAttemptStore::new(directory)?) as Arc<dyn AttemptStore>)
            }
            _ => None,
        };
        for (origin, cookie) in validated_cookies {
            self.jar.add_cookie_str(&cookie, &origin);
        }
        let mut client = Client::with_password_payer(transport, self.payer);
        client.attempts = store;
        Ok(client)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::cookie::CookieStore;
    #[test]
    fn cookie_headers_preserve_all_values_without_implicit_storage() {
        let jar = Arc::new(CookieJar::default());
        let client = Client::builder()
            .cookie_jar(jar.clone())
            .cookie_header("https://cashier.cc-pay.cn", "first=one; second=two==")
            .build()
            .unwrap();
        let cookies = jar
            .cookies(&url::Url::parse("https://cashier.cc-pay.cn/transaction").unwrap())
            .unwrap();
        let cookies = cookies.to_str().unwrap();
        assert!(cookies.contains("first=one") && cookies.contains("second=two=="));
        assert!(client.attempts.is_none());
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
    #[test]
    fn failed_build_does_not_partially_mutate_shared_cookie_jar() {
        let jar = Arc::new(CookieJar::default());
        let origin = url::Url::parse("https://cashier.cc-pay.cn").unwrap();
        assert!(
            Client::builder()
                .cookie_jar(jar.clone())
                .cookie_header(origin.as_str(), "valid=one; invalid")
                .build()
                .is_err()
        );
        assert!(jar.cookies(&origin).is_none());
    }
    #[test]
    fn explicit_storage_selection_uses_the_last_configuration() {
        let dir = tempfile::tempdir().unwrap();
        let unused = dir.path().join("unused");
        let memory: Arc<dyn AttemptStore> = Arc::new(crate::MemoryAttemptStore::default());
        let client = Client::builder()
            .state_directory(&unused)
            .attempt_store(memory.clone())
            .build()
            .unwrap();
        assert!(!unused.exists());
        assert!(Arc::ptr_eq(client.attempts.as_ref().unwrap(), &memory));
        let durable = dir.path().join("claims");
        let client = Client::builder()
            .attempt_store(memory)
            .state_directory(&durable)
            .build()
            .unwrap();
        client
            .claim_attempt("https://cashier.cc-pay.cn/cashier?id=T")
            .unwrap();
        let reopened = FileAttemptStore::new(durable).unwrap();
        assert!(reopened.contains("T").unwrap());
    }
}
