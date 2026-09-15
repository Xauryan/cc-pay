use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize)]
pub struct Payment {
    pub method: String,
    pub status: String,
    pub amount: String,
    pub cashier_url: String,
    pub qr_png: Option<String>,
    pub created_at: String,
    pub expires_policy: String,
    pub account_attempt: Option<String>,
    pub password_submitted: bool,
    #[serde(default)]
    pub automatic: Option<AutomaticPayment>,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct AutomaticPayment {
    pub outcome: String,
    pub debit_submitted: bool,
}
impl Payment {
    pub fn needs_confirmation(&self) -> bool {
        self.automatic.as_ref().is_some_and(|attempt| {
            !matches!(
                attempt.outcome.as_str(),
                "not_submitted" | "rejected" | "verified"
            )
        })
    }
}
