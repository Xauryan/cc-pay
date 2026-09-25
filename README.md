# cc-pay

**供其他 Rust 项目引用和参考的校园付 API 库。**

[简体中文](README.md) · [English](README.en.md)

从已有校园付订单和认证会话出发，处理微信、支付宝、数字人民币支付及订单查询。
本项目的核心是库接口：不提供 CLI 或常驻服务，不接管宿主项目的配置、日志、运行时和部署。

## 包与职责

| 包 | 能力 | 依赖边界 |
| --- | --- | --- |
| `cc-pay` | 会话、HTTP API、扫码、数字人民币、防重和结果核对 | 不依赖 Playwright、Node 或脚本文件 |
| `cc-pay-playwright` | 实现 `PasswordPayer`，可选支付宝密码付款 | Rust Playwright binding；调用方传入 `Browser` |

只引用 `cc-pay` 不会构建浏览器适配库。项目不包含商户下单、商户签名生成、机构账号登录、CLI 参数解析或环境变量加载。

## 引用

尚未发布到 crates.io，使用 Git 依赖；生产集成建议固定 `rev`：

```toml
[dependencies]
cc-pay = { git = "https://github.com/Xauryan/cc-pay" }
```

Cargo 包名是 `cc-pay`，Rust 导入名是 `cc_pay`。使用支持 Rust 2024 的工具链；可选 Playwright 适配库所用 binding 要求 Rust 1.88 或更高版本。

## 在宿主项目中调用

示例由调用方传入订单、会话和金额，不要求约定的环境变量、当前工作目录或可执行程序入口。

```rust,no_run
use cc_pay::{Client, Payment, PaymentMethod, PaymentOptions};

pub async fn wechat_payment(
    cashier_url: &str,
    cookie_header: &str,
    expected_amount: &str,
) -> anyhow::Result<Payment> {
    let client = Client::builder()
        .cookie_header("https://cashier.cc-pay.cn", cookie_header)
        .build()?;

    client.create_payment(cashier_url, PaymentMethod::Wechat, PaymentOptions {
        expected_amount: Some(expected_amount),
        ..Default::default()
    }).await
}
```

`qr_png` 是 Base64 编码的 PNG，由宿主决定如何展示。支付宝扫码使用 `PaymentMethod::Alipay` 且不传密码。
`transaction` 查询校园付订单，`payment_ways` 查询可用支付渠道。

## 会话和 HTTP 传输

`cookie_header` 接受 `name=value; other=value` 格式。使用 `cookie_jar` 共享现有 `Arc<CookieJar>`。
通过 `sso_origin` 显式允许 CAS origin，再调用 `authenticate` 复用已有 SSO 会话；本库不处理交互登录。

```rust,no_run
use cc_pay::{Client, CookieJar, PaymentClient};
use std::sync::Arc;

pub fn payment_client(jar: Arc<CookieJar>, proxy: &str) -> anyhow::Result<PaymentClient> {
    Client::builder()
        .cookie_jar(jar)
        .proxy(proxy)
        .sso_origin("https://sso.example.org")
        .build()
}
```

内置 `HttpTransport` 支持 HTTP / HTTPS / SOCKS5 / SOCKS5H 代理及认证，不继承系统代理、不在失败时直连。
它限制请求目标为获准的 HTTPS 443 地址，限制响应大小，并禁用透明重试和自动重定向。
`Client::new(custom_transport)` 可接入宿主 HTTP 实现；自定义实现须遵守 `Transport` 的 Cookie、目标校验和禁止重放约定。

## 自动付款与防重

**自动付款必须显式配置 `AttemptStore`。** 默认客户端可查询和生成扫码入口，不创建目录；没有存储时，支付宝密码付款和数字人民币付款会在任何网络请求前返回错误。

```rust,no_run
use cc_pay::{AttemptStore, Client, PaymentClient};
use std::sync::Arc;

pub fn with_shared_store(store: Arc<dyn AttemptStore>) -> anyhow::Result<PaymentClient> {
    Client::builder().attempt_store(store).build()
}
```

- 多台服务器：实现 `AttemptStore`，用数据库唯一约束原子持久化订单 claim。
- 本地持久化：显式调用 `.state_directory(application_owned_path)`，使用 `FileAttemptStore`。
- 测试或明确接受进程内保护：显式注入 `MemoryAttemptStore`；它不跨重启保留记录。

同一订单的自动付款 claim 不会自动释放。查询错误、取消任务或结果未知都不能成为重新扣款的理由。
传入 `PaymentOptions.expected_amount` 按十进制定点校验金额；请勿在业务层用浮点四舍五入代替金额核对。

数字人民币使用 `PaymentMethod::Ecny`，先在官方 App 绑定商户子钱包。`ecny_wallet_index = 0` 按上游顺序单轮尝试；`1..=99` 只尝试对应钱包。只有明确拒绝且核实订单仍未支付才会换钱包；超时或未知结果立即停止。

## 可选支付宝密码付款

额外引用 `cc-pay-playwright`，把宿主已有的 `playwright_rs::Browser` 传入 `PlaywrightPayer::new(browser)`，通过 `.password_payer(payer)` 注入客户端。完整可编译示例、运行时和代理边界见 [适配库文档](crates/cc-pay-playwright/README.md)。

核心库只认识 `PasswordPayer` 和 `PasswordPayment`。宿主可以实现自己的适配器，无需安装 Playwright。
适配器只能返回未提交、明确拒绝或结果待确认；最终成功必须由校园付订单确认。

Playwright 的 Rust binding 底层仍使用 Node.js driver。项目移除了自有 TypeScript worker、stdin/stdout JSON 协议、`CC_PAY_NODE` / `CC_PAY_WORKER` / `CC_PAY_BROWSER` 配置和编译期脚本路径。可选适配库使用支持请求拦截的 `playwright-rs 0.18.1`；不在付款调用中安装或启动 driver / 浏览器。

## 结果处理

| 结果 | 调用方处理 |
| --- | --- |
| `status == "success"` | 校园付已确认付款 |
| `needs_confirmation()` | 只查询 `transaction`，必要时人工核对 |
| `qr_png` 存在 | 展示二维码并查询订单状态 |
| `automatic.outcome == "rejected"` | 明确失败；如返回二维码则可展示 |
| 错误或其他状态 | 查询、核对，避免盲目重试 |

不记录密码、Cookie、签名 URL 或上游原始响应；宿主也应避免对这些值启用日志、浏览器 trace 或 HAR。

## 从 0.2 迁移

此次是 0.3 API 边界调整：

1. 移除 `BrowserPayer` / `BrowserResult` / `.alipay_browser(...)`，改用 `cc_pay_playwright::PlaywrightPayer` 或自定义 `PasswordPayer`；返回 `PasswordPayment`。
2. `ClientBuilder<P>` 和 `PaymentClient<P>` 保留注入的付款适配器类型，默认 `NoPasswordPayer`。
3. 默认不再写 `.cc-pay/attempts`，**继续显式使用旧目录或原有共享存储**；迁移时不得清空防重记录。
4. `.proxy(...)` 只配置 HTTP。宿主负责给浏览器配置相同出口；认证 SOCKS 代理需要宿主提供 relay。删除原 worker 部署步骤。

## 开发验证

```sh
cargo test --locked -p cc-pay
PLAYWRIGHT_SKIP_DRIVER_DOWNLOAD=1 cargo test --locked --workspace
PLAYWRIGHT_SKIP_DRIVER_DOWNLOAD=1 cargo clippy --locked --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
```

常规测试覆盖模拟 API、付款状态、防重及路由策略，不发起真实付款。可选本地浏览器测试使用内存页面和模拟路由，见适配库文档。测试通过不代表支付宝或数字人民币的真实扣款验收。
