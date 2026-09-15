use crate::{AlipayCredentials, Fields, PasswordPayer};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::{process::Stdio, sync::LazyLock, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::Command,
    sync::Semaphore,
};
use zeroize::Zeroizing;

// A browser is much heavier than a protocol request. Bound memory consumption
// across all users, while each attempt has its own process and cookie context.
static CAPACITY: LazyLock<Semaphore> = LazyLock::new(|| Semaphore::new(1));

#[derive(Serialize)]
struct Input<'a> {
    protocol: u8,
    action: &'a str,
    fields: &'a Fields,
    amount: &'a str,
    login: &'a str,
    password: &'a str,
    proxy: Option<&'a str>,
}

#[derive(Clone, Deserialize)]
pub struct BrowserResult {
    pub protocol: u8,
    pub outcome: String,
    pub password_submitted: bool,
    pub debit_submitted: bool,
    pub reason: String,
}

impl BrowserResult {
    pub fn unavailable(reason: &str) -> Self {
        Self {
            protocol: 1,
            outcome: "not_submitted".into(),
            password_submitted: false,
            debit_submitted: false,
            reason: reason.into(),
        }
    }
    fn uncertain() -> Self {
        Self {
            outcome: "uncertain".into(),
            reason: "worker_failure".into(),
            ..Self::unavailable("worker_failure")
        }
    }
    pub fn message(&self) -> &'static str {
        match self.reason.as_str() {
            "provider_rejected" => "支付宝拒绝本次密码付款，将回退扫码",
            "interactive_required" => "支付宝需要额外验证或人工操作，将回退扫码",
            "component_unavailable" => "支付宝安全组件未就绪，将回退扫码",
            "invalid_order" => "支付表单或金额校验未通过，将回退扫码",
            "busy" => "自动付款队列等待超时，将回退扫码",
            "timeout" => "自动付款在扣款提交前超时，将回退扫码",
            "confirmation_required" | "worker_failure" => {
                "自动付款结果待确认，请核对订单；暂不重复付款或生成扫码入口"
            }
            _ => "支付浏览器暂不可用，将回退扫码",
        }
    }
    fn validate(self) -> Option<Self> {
        (self.protocol == 1
            && matches!(
                self.outcome.as_str(),
                "not_submitted" | "rejected" | "uncertain"
            )
            && (!self.debit_submitted || self.outcome == "uncertain"))
            .then_some(self)
    }
}

#[cfg(unix)]
struct ProcessGroup(u32);
#[cfg(unix)]
impl Drop for ProcessGroup {
    fn drop(&mut self) {
        // The child owns a new process group. Kill only that group, including
        // Chromium descendants, on timeout, cancellation, and normal shutdown.
        if self.0 > 1 && self.0 <= i32::MAX as u32 {
            unsafe {
                libc::kill(-(self.0 as i32), libc::SIGKILL);
            }
        }
    }
}

impl PasswordPayer for BrowserPayer {
    async fn pay(
        &self,
        action: &str,
        fields: &Fields,
        amount: &str,
        credentials: AlipayCredentials<'_>,
    ) -> BrowserResult {
        let Ok(Ok(_permit)) =
            tokio::time::timeout(Duration::from_secs(60), CAPACITY.acquire()).await
        else {
            return BrowserResult::unavailable("busy");
        };
        let input = Input {
            protocol: 1,
            action,
            fields,
            amount,
            login: credentials.account,
            password: credentials.password,
            proxy: self.proxy.as_deref(),
        };
        let Ok(bytes) = serde_json::to_vec(&input) else {
            return BrowserResult::unavailable("invalid_order");
        };
        let bytes = Zeroizing::new(bytes);
        let mut cmd = Command::new(&self.node);
        cmd.arg(&self.worker)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        // Never inherit debugging that might log request bodies or proxy secrets.
        for name in [
            "DEBUG",
            "PWDEBUG",
            "NODE_OPTIONS",
            "NODE_DEBUG",
            "NODE_USE_ENV_PROXY",
            "HTTP_PROXY",
            "HTTPS_PROXY",
            "ALL_PROXY",
            "NO_PROXY",
            "http_proxy",
            "https_proxy",
            "all_proxy",
            "no_proxy",
        ] {
            cmd.env_remove(name);
        }
        #[cfg(unix)]
        cmd.process_group(0);
        let Ok(mut child) = cmd.spawn() else {
            return BrowserResult::unavailable("browser_unavailable");
        };
        #[cfg(unix)]
        let _group = ProcessGroup(child.id().unwrap_or_default());
        let Some(mut stdin) = child.stdin.take() else {
            return BrowserResult::uncertain();
        };
        let Some(stdout) = child.stdout.take() else {
            return BrowserResult::uncertain();
        };
        let work = async {
            stdin.write_all(&bytes).await.ok()?;
            stdin.shutdown().await.ok()?;
            drop(stdin);
            let mut output = Vec::new();
            stdout.take(16385).read_to_end(&mut output).await.ok()?;
            let status = child.wait().await.ok()?;
            if !status.success() || output.len() > 16384 {
                return None;
            }
            serde_json::from_slice::<BrowserResult>(&output)
                .ok()?
                .validate()
        };
        match tokio::time::timeout(Duration::from_secs(95), work).await {
            Ok(Some(result)) => result,
            _ => BrowserResult::uncertain(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn transport_failure_cannot_become_safe_to_retry() {
        assert_eq!(BrowserResult::uncertain().outcome, "uncertain");
        let invalid = BrowserResult {
            debit_submitted: true,
            ..BrowserResult::unavailable("timeout")
        };
        assert!(invalid.validate().is_none());
        assert!(BrowserResult::uncertain().validate().is_some());
    }
}

/// Isolated official Alipay component runner. Secrets travel through stdin only.
#[derive(Clone)]
pub struct BrowserPayer {
    pub node: PathBuf,
    pub worker: PathBuf,
    pub proxy: Option<String>,
}
impl Default for BrowserPayer {
    fn default() -> Self {
        Self {
            node: std::env::var_os("CC_PAY_NODE")
                .map(Into::into)
                .unwrap_or_else(|| "node".into()),
            worker: std::env::var_os("CC_PAY_WORKER")
                .map(Into::into)
                .unwrap_or_else(|| {
                    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("payment-worker/worker.ts")
                }),
            proxy: None,
        }
    }
}
