# cc-pay

**校园付 Rust API 库 · 微信、支付宝与数字人民币**

[简体中文](README.md) · [English](README.en.md)

![Rust](https://img.shields.io/badge/Rust-2024-000000?logo=rust)
![Tokio](https://img.shields.io/badge/Async-Tokio-463D5E)
![Reqwest](https://img.shields.io/badge/HTTP-Reqwest%20%2B%20Rustls-009688)
![Serde](https://img.shields.io/badge/JSON-Serde-E57324)
![TypeScript](https://img.shields.io/badge/TypeScript-strict-3178C6?logo=typescript&logoColor=white)
![Playwright](https://img.shields.io/badge/Browser-Playwright-2EAD33)
![Node.js](https://img.shields.io/badge/Node.js-%3E%3D24.12-5FA04E?logo=nodedotjs&logoColor=white)

从已有校园付订单和认证会话出发，统一处理支付渠道、二维码、代理及付款状态。可直接嵌入 Rust 服务，也可通过 trait 接入自己的 HTTP 客户端与防重存储。

## 功能

| 支付方式 | 能力 | 运行依赖 |
| --- | --- | --- |
| 微信 | 生成扫码二维码、查询订单 | Rust |
| 支付宝 | 扫码付款；可选密码付款与扫码回退 | Rust；密码付款另需 Node.js + Chromium |
| 数字人民币 | 已绑定子钱包付款；指定钱包或按顺序单轮尝试 | Rust |

- 复用 Cookie 会话，支持已有 CAS SSO 会话认证。
- HTTP / HTTPS / SOCKS5 / SOCKS5H 代理，支持认证与内网地址。
- 定点金额校验、持久化付款防重、未知结果保护。
- 支付宝密码组件在独立浏览器上下文中运行，凭据通过 stdin 传递。

本库不负责商户下单、生成商户签名或机构账号登录。支付宝自动付款仍取决于官方安全组件、账户状态及额外验证要求。

## 安装

使用支持 Rust 2024 edition 的稳定工具链。目前通过 Git 引用，尚未发布到 crates.io：

```toml
[dependencies]
cc-pay = { git = "https://github.com/Xauryan/cc-pay" }
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
anyhow = "1"
```

Cargo 包名为 `cc-pay`，Rust 导入名为 `cc_pay`。生产项目可用 `rev` 固定 Git 提交。

## 快速开始

```rust,no_run
use cc_pay::{Client, PaymentMethod, PaymentOptions};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cashier = std::env::var("CC_PAY_CASHIER_URL")?;
    let cookie = std::env::var("CC_PAY_COOKIE")?;
    let client = Client::builder()
        .cookie_header("https://cashier.cc-pay.cn", cookie)
        .state_directory("./.cc-pay/attempts")
        .build()?;

    let transaction = client.transaction(&cashier).await?;
    println!("status={}", transaction["status"]);

    let payment = client.create_payment(
        &cashier,
        PaymentMethod::Wechat,
        PaymentOptions {
            expected_amount: Some("1.00"),
            ..Default::default()
        },
    ).await?;

    // qr_png 是 PNG 的 Base64；由调用方展示给付款人。
    if let Some(png) = payment.qr_png {
        let _image_source = format!("data:image/png;base64,{png}");
    }
    Ok(())
}
```

改用 `PaymentMethod::Alipay` 即可生成支付宝扫码入口。示例中的环境变量由调用方读取；库不会自动加载 `.env`。Cookie 格式为 `name=value; other=value`，也可通过 `cookie_jar` 共享已有 `Arc<CookieJar>`。

## 支付宝密码付款

只在使用密码付款时安装可选组件。准备 **Node.js ≥ 24.12** 和 pnpm，在仓库或 `.crate` 解包目录执行：

```sh
cd payment-worker
pnpm install --frozen-lockfile
pnpm exec playwright-core install chromium --only-shell
pnpm check
```

Linux 缺少浏览器系统依赖时，可为安装命令加 `--with-deps`。组件使用 [Node.js 原生 TypeScript 支持](https://nodejs.org/api/typescript.html)，不需要生成 JavaScript 文件。`pnpm check` 仅检查本地浏览器启动，不发起付款。

```rust,no_run
use cc_pay::{AlipayCredentials, BrowserPayer, Client, PaymentMethod, PaymentOptions};

# async fn example() -> anyhow::Result<()> {
let client = Client::builder()
    .cookie_header("https://cashier.cc-pay.cn", std::env::var("CC_PAY_COOKIE")?)
    .alipay_browser(BrowserPayer::default())
    .build()?;
let cashier = std::env::var("CC_PAY_CASHIER_URL")?;
let account = std::env::var("CC_PAY_ALIPAY_ACCOUNT")?;
let password = std::env::var("CC_PAY_ALIPAY_PASSWORD")?;
let payment = client.create_payment(&cashier, PaymentMethod::Alipay, PaymentOptions {
    alipay: Some(AlipayCredentials { account: &account, password: &password }),
    expected_amount: Some("1.00"),
    ..Default::default()
}).await?;
# Ok(())
# }
```

| 组件环境变量 | 用途 |
| --- | --- |
| `CC_PAY_NODE` | Node.js 可执行文件，默认 `node` |
| `CC_PAY_WORKER` | `worker.ts` 的绝对路径；默认使用编译时的包目录 |
| `CC_PAY_BROWSER` | 已安装 Chromium 的可执行文件路径，可省略浏览器下载 |

部署二进制时，同时部署 `payment-worker` 及其依赖，并设置 `CC_PAY_WORKER`。不传支付宝凭据时使用扫码；明确拒绝或提交前失败时，在核对订单后尝试回退扫码。扣款已提交或结果未知时只查询状态。

## 数字人民币

先在数字人民币 App 中绑定商户子钱包。使用 `PaymentMethod::Ecny`，并通过 `PaymentOptions.ecny_wallet_index` 选择：

- `0`：按 `payment_ways` 返回的 `ecCode` 顺序，每个钱包最多尝试一次。
- `1..=99`：仅尝试对应序号的钱包；不存在时在扣款前停止。

只在明确拒绝且订单仍未付款时切换钱包。超时或结果不明时停止；成功依据校园付订单确认，不生成数币二维码。

## 会话与代理

```rust,no_run
use cc_pay::Client;

# async fn example() -> anyhow::Result<()> {
let client = Client::builder()
    .proxy("socks5h://user:password@127.0.0.1:1080")
    .sso_origin("https://sso.example.org")
    .cookie_header("https://sso.example.org", "SESSION=existing-session")
    .build()?;
let cashier = std::env::var("CC_PAY_CASHIER_URL")?;
client.authenticate(&cashier, "https://sso.example.org/login").await?;
# Ok(())
# }
```

代理同时用于 Rust 请求和支付宝浏览器，失败不会回退直连。网络请求限制在授权的 HTTPS 443 域名内，禁用透明重试和自动跳转；业务层只跟随经过验证的跳转。

## 付款结果与防重

| 结果 | 调用方处理 |
| --- | --- |
| `status == "success"` | 校园付已确认付款 |
| `needs_confirmation() == true` | 仅查询 `transaction`，必要时人工核对 |
| `qr_png` 存在 | 展示二维码，随后查询订单状态 |
| `automatic.outcome == "rejected"` | 自动付款明确失败；若存在二维码，可展示扫码入口 |
| 其他状态或错误 | 查询订单或转人工处理，避免盲目重试 |

`Client::builder()` 默认将自动付款防重记录写入 `.cc-pay/attempts`。同一订单只允许启动一轮自动付款，记录不会自动释放。多进程须共享同一本地目录；多台服务器通过 `AttemptStore` 接入具有唯一约束的共享存储。高级接口 `Client::new` 默认使用内存存储，需自行配置持久化。

**从旧版迁移时**，更新包名、导入名和组件环境变量，并通过 `state_directory` 继续使用原有防重目录，或停机迁移已有记录；不要通过清空记录重试未确认的付款。密码、Cookie、签名 URL 和付款原始响应不应写入日志。

## API 概览

| 接口 | 用途 |
| --- | --- |
| `Client::builder()` / `PaymentClient` | 默认客户端、会话、代理和持久化防重 |
| `transaction` / `payment_ways` | 查询订单及可用渠道 |
| `authenticate` | 使用已有 CAS SSO 会话认证 |
| `create_payment` | 创建扫码入口或发起自动付款 |
| `BrowserPayer` / `PasswordPayer` | 内置支付宝组件或自定义实现 |
| `Transport` / `AttemptStore` | 自定义 HTTP 与共享防重存储 |

## 结构与验证

```text
src/              Rust API、支付流程、会话与防重
payment-worker/   可选 TypeScript 支付宝密码付款组件
README.md         中文文档
README.en.md      English documentation
```

```sh
cargo test --locked
pnpm --dir payment-worker typecheck
pnpm --dir payment-worker check
```

保留与核心行为直接相关的源码内单元测试和文档示例编译检查。项目不包含独立演示程序或平台专用安装脚本。
