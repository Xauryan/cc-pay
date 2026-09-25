# cc-pay

**供其他 Rust 项目引用和参考的校园付 API 库。**

[简体中文](README.md) · [English](README.en.md)

cc-pay 提供微信扫码、支付宝扫码与密码付款、数字人民币钱包付款及订单查询接口。调用方传入已有校园付订单和认证会话，在宿主应用中配置运行时、代理、存储和结果展示。

## 包与功能

| 包 | 功能 | 接入方式 |
| --- | --- | --- |
| `cc-pay` | 会话、HTTP API、二维码、数字人民币、防重和结果核对 | 构建 `Client`，传入会话及付款参数 |
| `cc-pay-playwright` | 支付宝密码付款 | 传入已有 `Browser`，通过 `PasswordPayer` 接入客户端 |

`cc-pay` 可独立引用。支付宝密码付款适配器的配置和运行要求见 [cc-pay-playwright](crates/cc-pay-playwright/README.md)。

## 引用

使用支持 Rust 2024 edition 的工具链，通过 Git 引用：

```toml
[dependencies]
cc-pay = { git = "https://github.com/Xauryan/cc-pay" }
anyhow = "1"
```

Cargo 包名为 `cc-pay`，Rust 导入名为 `cc_pay`。生产集成可通过 Git 依赖的 `rev` 固定提交。异步请求在宿主提供的 Tokio 运行时中执行。

## 创建扫码付款

以下示例接收收银台 URL、Cookie 请求头和预期金额，在宿主的 Tokio 运行时中调用：

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

`Payment.qr_png` 是 Base64 编码的 PNG，宿主可将其转换为 `data:image/png;base64,...` 展示。支付宝扫码使用 `PaymentMethod::Alipay`，并保持 `PaymentOptions.alipay` 为 `None`。

## 客户端与会话

`Client::builder()` 使用内置 `HttpTransport`。`ClientBuilder<P>` 和 `PaymentClient<P>` 的类型参数 `P` 表示密码付款适配器，默认值为 `NoPasswordPayer`。

| 配置方法 | 用途 |
| --- | --- |
| `cookie_jar(jar)` | 共享宿主的 `Arc<CookieJar>` |
| `cookie_header(origin, header)` | 为指定 origin 写入 `name=value; other=value` 格式的 Cookie |
| `sso_origin(origin)` | 允许一个 CAS SSO origin |
| `proxy(url)` | 设置 HTTP 传输使用的代理 |
| `user_agent(value)` | 设置 HTTP User-Agent |
| `attempt_store(store)` | 注入 `Arc<dyn AttemptStore>` |
| `state_directory(path)` | 使用指定本地目录保存付款防重记录 |
| `password_payer(payer)` | 注入实现 `PasswordPayer` 的付款适配器 |

Cookie origin 和 SSO origin 使用 HTTPS 443，路径为 `/`，查询参数和 fragment 为空。宿主提供已认证会话；`authenticate(cashier_url, sso_login_url)` 复用该会话完成校园付 SSO 跳转。

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

`HttpTransport` 支持 HTTP、HTTPS、SOCKS5 和 SOCKS5H 代理及认证。代理由 `proxy` 显式配置，代理请求失败时返回错误。内置传输的连接超时为 5 秒、请求超时为 15 秒、响应体上限为 8 MiB；请求目标限定在授权的 HTTPS 443 地址内。自动重试和自动重定向处于禁用状态，支付流程逐次验证并跟随跳转。

HTTP 客户端和浏览器各自配置代理，宿主负责保持两者的出口策略一致。

## 自动付款与防重存储

**支付宝密码付款和数字人民币付款必须显式配置 `AttemptStore`。** 缺少存储时，`create_payment` 在发起网络请求前返回错误。查询和扫码付款可使用默认客户端。

```rust,no_run
use cc_pay::{AttemptStore, Client, PaymentClient};
use std::sync::Arc;

pub fn with_shared_store(store: Arc<dyn AttemptStore>) -> anyhow::Result<PaymentClient> {
    Client::builder().attempt_store(store).build()
}
```

| 存储方式 | 适用场景 | 行为 |
| --- | --- | --- |
| 自定义 `AttemptStore` | 多台应用服务器 | 通过共享存储和唯一约束，原子持久化订单占用记录 |
| `FileAttemptStore` / `state_directory(path)` | 使用同一本地目录的应用进程 | 通过原子文件创建保存记录，重启后继续生效 |
| `MemoryAttemptStore` | 测试或进程内保护 | 记录保存在所共享的存储实例中，生命周期随进程结束 |

`AttemptStore::contains` 查询记录，`claim` 为每个订单最多返回一次 `true`。持久化实现应在 `claim` 返回成功前完成落盘。`attempt_store` 和 `state_directory` 以最后一次配置为准；本地目录在 `build()` 时创建。

自动付款占用记录按订单保存，并用于阻止该订单后续的付款创建请求，覆盖各支付方式。记录持续保留；查询错误、任务取消或结果未知时，后续操作应限于订单查询和付款核对。应用重启和多实例运行期间应持续使用同一组防重记录。

`PaymentOptions.expected_amount` 接受十进制金额字符串，金额比较使用整数分计算。自动扣款前会校验预期金额；金额不符时停止扣款。

## 数字人民币

先在数字人民币 App 绑定商户子钱包，再使用 `PaymentMethod::Ecny`。`PaymentOptions.ecny_wallet_index` 控制钱包选择：

| 值 | 行为 |
| --- | --- |
| `0`，默认值 | 按 `payment_ways` 返回的 `ecCode` 顺序，每个钱包尝试一次 |
| `1..=99` | 仅尝试对应序号的钱包；序号超出已绑定钱包数量时停止付款准备 |

钱包切换要求上游明确拒绝，并确认同一订单仍处于待支付状态。超时或未知结果会停止后续尝试。付款成功以校园付订单状态为准。

## 支付宝密码付款

引用 `cc-pay-playwright`，将宿主已有的 `playwright_rs::Browser` 传给 `PlaywrightPayer::new(browser)`，再通过 `password_payer` 注入客户端。调用 `create_payment` 时，将支付宝账号和六位支付密码作为 `AlipayCredentials` 放入 `PaymentOptions.alipay`。

适配库使用 `playwright-rs 0.18.1`，运行环境包含 Playwright driver、Node.js 和 Chromium。宿主负责运行环境准备、浏览器启动、代理、日志及关闭。完整示例与配置要求见 [适配库文档](crates/cc-pay-playwright/README.md)。

## 结果处理

`create_payment` 返回 `Payment`，其中包含付款方式、订单状态、金额、二维码及自动付款结果。

| 结果 | 调用方处理 |
| --- | --- |
| `status == "success"` | 校园付已确认付款 |
| `needs_confirmation()` 返回 `true` | 查询 `transaction`，必要时人工核对 |
| `qr_png` 存在 | 展示二维码并查询订单状态 |
| `automatic.outcome == "rejected"` | 自动付款明确失败；如返回二维码则可展示 |
| 错误或其他状态 | 查询订单并核对结果后处理 |

`automatic.debit_submitted` 记录扣款请求是否已提交，`password_submitted` 记录密码请求是否已提交。宿主的日志、浏览器 trace 和 HAR 应排除密码、Cookie、签名 URL 及上游原始响应。

## 查询与扩展接口

`transaction(cashier_url)` 查询订单，`payment_ways(cashier_url)` 查询可用支付渠道。两者均返回 `serde_json::Value`，便于调用方读取上游字段。

| 接口 | 契约 |
| --- | --- |
| `Transport` | 共享 Cookie、校验目标地址、限制响应大小，并禁用自动重试和重定向 |
| `AttemptStore` | 查询并原子记录订单的自动付款尝试，存储错误会阻止付款 |
| `PasswordPayer` | 接收签名表单、金额和凭据，返回 `PasswordPayment` |

`Client::new(transport)` 接入自定义传输，`Client::with_password_payer(transport, payer)` 同时接入自定义付款适配器。通过 `with_attempt_store(store)` 配置防重存储。`PasswordPayer` 支持 `Arc<P>` 和 `Option<P>` 包装。

`PasswordPayment` 提供 `not_submitted`、`rejected` 和 `uncertain` 构造方法，以及状态、原因和提交标记的读取方法。扣款可能已提交时应返回 `uncertain`；校园付订单状态负责确认最终成功。

## 开发验证

在仓库根目录执行：

```sh
cargo test --locked -p cc-pay
PLAYWRIGHT_SKIP_DRIVER_DOWNLOAD=1 cargo test --locked --workspace
PLAYWRIGHT_SKIP_DRIVER_DOWNLOAD=1 cargo clippy --locked --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
```

常规测试使用模拟 API，覆盖付款状态、防重、金额校验及路由策略。文档中的 Rust 示例参与编译检查。本地浏览器测试使用内存页面和模拟响应，运行方式见适配库文档。真实账户及支付渠道的验收由宿主集成环境单独执行。
