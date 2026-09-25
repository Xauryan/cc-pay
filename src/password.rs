//! Runtime-independent automatic payment results. No worker wire protocol.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PasswordOutcome {
    NotSubmitted,
    Rejected,
    Uncertain,
}
impl PasswordOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotSubmitted => "not_submitted",
            Self::Rejected => "rejected",
            Self::Uncertain => "uncertain",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PasswordReason {
    Unavailable,
    InvalidOrder,
    Busy,
    Timeout,
    ProviderRejected,
    InteractiveRequired,
    ComponentUnavailable,
    ConfirmationRequired,
}

/// An adapter cannot claim verified success: only the campus transaction can.
/// Private fields prevent a submitted debit from being marked safe for fallback.
#[derive(Clone, Debug)]
pub struct PasswordPayment {
    outcome: PasswordOutcome,
    password_submitted: bool,
    debit_submitted: bool,
    reason: PasswordReason,
}
impl PasswordPayment {
    pub fn not_submitted(reason: PasswordReason) -> Self {
        if reason == PasswordReason::ConfirmationRequired {
            return Self::uncertain(false, false);
        }
        Self {
            outcome: PasswordOutcome::NotSubmitted,
            password_submitted: false,
            debit_submitted: false,
            reason,
        }
    }
    pub fn rejected(password_submitted: bool) -> Self {
        Self {
            outcome: PasswordOutcome::Rejected,
            password_submitted,
            debit_submitted: false,
            reason: PasswordReason::ProviderRejected,
        }
    }
    /// Use whenever a debit may have been sent, even if its dispatch is unknown.
    pub fn uncertain(password_submitted: bool, debit_submitted: bool) -> Self {
        Self {
            outcome: PasswordOutcome::Uncertain,
            password_submitted,
            debit_submitted,
            reason: PasswordReason::ConfirmationRequired,
        }
    }
    pub fn with_password_submitted(mut self, submitted: bool) -> Self {
        self.password_submitted |= submitted;
        self
    }
    pub fn outcome(&self) -> PasswordOutcome {
        self.outcome
    }
    pub fn reason(&self) -> PasswordReason {
        self.reason
    }
    pub fn password_submitted(&self) -> bool {
        self.password_submitted
    }
    pub fn debit_submitted(&self) -> bool {
        self.debit_submitted
    }
    pub fn message(&self) -> &'static str {
        match self.reason {
            PasswordReason::ProviderRejected => "支付宝拒绝本次密码付款，将回退扫码",
            PasswordReason::InteractiveRequired => "支付宝需要额外验证或人工操作，将回退扫码",
            PasswordReason::ComponentUnavailable => "支付宝安全组件未就绪，将回退扫码",
            PasswordReason::InvalidOrder => "支付表单或金额校验未通过，将回退扫码",
            PasswordReason::Busy => "自动付款队列等待超时，将回退扫码",
            PasswordReason::Timeout => "自动付款在扣款提交前超时，将回退扫码",
            PasswordReason::ConfirmationRequired => {
                "自动付款结果待确认，请核对订单；暂不重复付款或生成扫码入口"
            }
            PasswordReason::Unavailable => "支付浏览器暂不可用，将回退扫码",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn confirmation_reason_never_becomes_a_retryable_result() {
        let result = PasswordPayment::not_submitted(PasswordReason::ConfirmationRequired);
        assert_eq!(result.outcome(), PasswordOutcome::Uncertain);
        assert!(!result.debit_submitted());
        assert_eq!(
            PasswordPayment::uncertain(true, true).outcome(),
            PasswordOutcome::Uncertain
        );
    }
}
