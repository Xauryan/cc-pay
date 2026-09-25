#![doc = include_str!("../README.md")]
mod attempts;
mod builder;
mod model;
mod password;
mod payment;
pub mod transport;

use anyhow::{Result, ensure};
pub use attempts::{AttemptStore, FileAttemptStore, MemoryAttemptStore};
pub use builder::{ClientBuilder, PaymentClient};
pub use model::{AutomaticPayment, Payment};
pub use password::{PasswordOutcome, PasswordPayment, PasswordReason};
pub use payment::{cashier_id, parse_post_form, png, valid_wallet_preference};
pub use reqwest::cookie::Jar as CookieJar;
use serde::{Deserialize, Serialize};
use std::{future::Future, sync::Arc};
pub use transport::{Fields, HttpTransport, Response, Transport};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PaymentMethod {
    Wechat,
    Alipay,
    Ecny,
}
impl PaymentMethod {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Wechat => "wechat",
            Self::Alipay => "alipay",
            Self::Ecny => "ecny",
        }
    }
}
impl std::str::FromStr for PaymentMethod {
    type Err = anyhow::Error;
    fn from_str(value: &str) -> Result<Self> {
        match value {
            "wechat" => Ok(Self::Wechat),
            "alipay" => Ok(Self::Alipay),
            "ecny" => Ok(Self::Ecny),
            _ => anyhow::bail!("支付方式无效"),
        }
    }
}

/// Intentionally not Debug/Serialize; avoid accidental credential logging.
#[derive(Clone, Copy)]
pub struct AlipayCredentials<'a> {
    pub account: &'a str,
    pub password: &'a str,
}

#[derive(Default)]
pub struct PaymentOptions<'a> {
    pub alipay: Option<AlipayCredentials<'a>>,
    /// Zero tries wallets in upstream order once; positive values select that 1-based wallet only.
    pub ecny_wallet_index: usize,
    /// Optional decimal amount, checked exactly before submitting an automatic debit.
    pub expected_amount: Option<&'a str>,
}

/// Implement an automatic payer without coupling the core to a browser runtime.
/// A returned uncertain result must never be retried or converted to QR payment.
pub trait PasswordPayer: Send + Sync {
    fn pay(
        &self,
        action: &str,
        fields: &Fields,
        amount: &str,
        credentials: AlipayCredentials<'_>,
    ) -> impl Future<Output = PasswordPayment> + Send;
}
#[derive(Clone, Default)]
pub struct NoPasswordPayer;
impl PasswordPayer for NoPasswordPayer {
    async fn pay(&self, _: &str, _: &Fields, _: &str, _: AlipayCredentials<'_>) -> PasswordPayment {
        PasswordPayment::not_submitted(PasswordReason::Unavailable)
    }
}

impl<P: PasswordPayer> PasswordPayer for Arc<P> {
    async fn pay(
        &self,
        action: &str,
        fields: &Fields,
        amount: &str,
        credentials: AlipayCredentials<'_>,
    ) -> PasswordPayment {
        self.as_ref().pay(action, fields, amount, credentials).await
    }
}
impl<P: PasswordPayer> PasswordPayer for Option<P> {
    async fn pay(
        &self,
        action: &str,
        fields: &Fields,
        amount: &str,
        credentials: AlipayCredentials<'_>,
    ) -> PasswordPayment {
        match self {
            Some(payer) => payer.pay(action, fields, amount, credentials).await,
            None => PasswordPayment::not_submitted(PasswordReason::Unavailable),
        }
    }
}

#[derive(Clone)]
pub struct Client<T = HttpTransport, P = NoPasswordPayer> {
    transport: T,
    password_payer: P,
    attempts: Option<Arc<dyn AttemptStore>>,
}
impl<T: Transport> Client<T> {
    pub fn new(transport: T) -> Self {
        Self::with_password_payer(transport, NoPasswordPayer)
    }
}
impl<T: Transport, P: PasswordPayer> Client<T, P> {
    pub fn with_password_payer(transport: T, password_payer: P) -> Self {
        Self {
            transport,
            password_payer,
            attempts: None,
        }
    }
    /// Override the attempt store before making any payment calls. Do not replace
    /// a store after use: previously recorded attempts must remain protected.
    pub fn with_attempt_store(mut self, store: Arc<dyn AttemptStore>) -> Self {
        self.attempts = Some(store);
        self
    }
    fn claim_attempt(&self, cashier: &str) -> Result<()> {
        ensure!(
            self.attempts
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("自动付款必须显式配置 AttemptStore"))?
                .claim(&cashier_id(cashier)?)?,
            "该订单已有自动付款尝试，请仅查询付款状态"
        );
        Ok(())
    }
    pub async fn payment_ways(&self, cashier: &str) -> Result<serde_json::Value> {
        let tx = self.transaction(cashier).await?;
        let goods = match &tx["goodsId"] {
            serde_json::Value::String(s) if !s.is_empty() => s.clone(),
            serde_json::Value::Number(n) => n.to_string(),
            _ => anyhow::bail!("校园付缺少 goodsId"),
        };
        self.cashier_get(
            "/api/pay_ways",
            &transport::fields(&[("goodsId", &goods), ("payScene", "")]),
        )
        .await
    }
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}
