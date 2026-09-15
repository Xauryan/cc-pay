use super::*;

pub const BANKS: &[(&str, &str)] = &[
    ("icbc", "工商银行"),
    ("abc", "农业银行"),
    ("boc", "中国银行"),
    ("ccb", "建设银行"),
    ("psbc", "邮储银行"),
    ("comm", "交通银行"),
    ("mybank", "网商银行"),
    ("cmb", "招商银行"),
    ("webank", "微众银行"),
];

pub fn valid_wallet_preference(index: usize) -> bool {
    index <= 99
}

fn wallet_codes(ways: &Value, index: usize) -> Result<Vec<(usize, String)>> {
    ensure!(valid_wallet_preference(index), "数币钱包序号无效");
    let wallets = ways["ecCode"]
        .as_array()
        .context("校园付未返回已绑定的数币钱包")?;
    ensure!(
        !wallets.is_empty(),
        "尚未绑定钱包，请在数字人民币 App 中绑定对应商户的子钱包，或到校园付收银台付款"
    );
    ensure!(
        index <= wallets.len(),
        "所选钱包序号超出已绑定钱包数量，请到校园付收银台核对钱包顺序"
    );
    let mut seen = std::collections::HashSet::new();
    let mut codes = Vec::new();
    for (position, wallet) in wallets.iter().enumerate() {
        let code = wallet.as_str().context("数币钱包列表格式无效")?;
        ensure!(
            !code.is_empty()
                && code.len() <= 32
                && code.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'),
            "数币钱包标识无效"
        );
        ensure!(seen.insert(code), "钱包列表含重复项，请到校园付收银台核对");
        if index == 0 || index == position + 1 {
            codes.push((position + 1, code.to_owned()));
        }
    }
    Ok(codes)
}

// A failed network request or an unknown business message is not evidence that
// no money moved. Rotate only on an explicit no-debit rejection, and then verify
// that this very transaction is still unpaid before touching the next wallet.
fn definitely_rejected(envelope: &Value, transaction_id: &str) -> bool {
    let payload = &envelope["data"];
    if envelope["success"] != false
        || payload["isPaid"] == true
        || payload
            .get("transactionId")
            .is_some_and(|id| id != "" && id != transaction_id)
    {
        return false;
    }
    let message = val(envelope, "message").to_lowercase();
    if [
        "超时",
        "未知",
        "处理中",
        "已支付",
        "timeout",
        "unknown",
        "processing",
    ]
    .iter()
    .any(|s| message.contains(s))
    {
        return false;
    }
    payload["isPaid"] == false
        || [
            "余额不足",
            "超过支付限额",
            "超出支付限额",
            "钱包已注销",
            "钱包已冻结",
            "insufficient funds",
            "insufficient balance",
        ]
        .iter()
        .any(|s| message.contains(s))
}

// Compare decimal amounts exactly; never round a floating-point value before a debit.
pub(super) fn cents(amount: &str) -> Option<u64> {
    let mut parts = amount.split('.');
    let integer = parts.next()?;
    let fraction = parts.next().unwrap_or("");
    if integer.is_empty()
        || integer.len() > 10
        || !integer.bytes().all(|b| b.is_ascii_digit())
        || fraction.len() > 2
        || !fraction.bytes().all(|b| b.is_ascii_digit())
        || parts.next().is_some()
    {
        return None;
    }
    let fractional: u64 = match fraction.len() {
        0 => 0,
        1 => fraction.parse::<u64>().ok()? * 10,
        _ => fraction.parse().ok()?,
    };
    integer
        .parse::<u64>()
        .ok()?
        .checked_mul(100)?
        .checked_add(fractional)
}

pub(crate) fn same_transaction(tx: &Value, cashier: &str, amount: Option<&str>) -> Result<()> {
    if let Some(id) = tx.get("id") {
        ensure!(
            id.as_str() == Some(cashier_id(cashier)?.as_str()),
            "校园付订单编号不匹配"
        );
    }
    ensure!(val(tx, "currency") == "CNY", "数币仅支持人民币订单");
    let money = scalar(tx, "money")?;
    let value = cents(&money).filter(|v| *v > 0).context("校园付金额无效")?;
    if let Some(amount) = amount {
        ensure!(
            cents(amount) == Some(value),
            "校园付订单金额发生变化，请到校园付收银台核对"
        );
    }
    Ok(())
}

impl<T: Transport, P: PasswordPayer> Client<T, P> {
    pub(super) async fn ecny_payment(
        &self,
        cashier: &str,
        preference: usize,
        expected_amount: Option<&str>,
    ) -> Result<Payment> {
        let mut payment = unsubmitted_payment(cashier, "ecny");
        payment.account_attempt =
            Some("数币付款尚未提交，校园付暂不可用；请查询订单状态或到校园付收银台付款".into());
        let tx = match self.transaction(cashier).await {
            Ok(tx) => tx,
            Err(_) => return Ok(payment),
        };
        if let Err(error) = same_transaction(&tx, cashier, expected_amount) {
            payment.account_attempt = Some(format!("本次未扣款：{error}"));
            return Ok(payment);
        }
        payment.amount = scalar(&tx, "money")?;
        payment.created_at = val(&tx, "tsCreation").into();
        payment.expires_policy = val(&tx, "expireDuration").into();
        payment.status = val(&tx, "status").into();
        if payment.status != "wait_payer_pay" {
            apply_transaction(&mut payment, &tx);
            payment.account_attempt =
                Some("校园付订单已不处于待付款状态，未提交新的数币付款".into());
            return Ok(payment);
        }
        let prepared = async {
            let ways = self
                .cashier_get(
                    "/api/pay_ways",
                    &fields(&[("goodsId", &scalar(&tx, "goodsId")?), ("payScene", "")]),
                )
                .await?;
            let codes = wallet_codes(&ways, preference)?;
            let channels = ways["normal"]
                .as_array()
                .context("支付渠道格式无效")?
                .iter()
                .filter(|v| {
                    val(v, "name") == "ecpay_token"
                        && v["isActive"] == true
                        && v["isDeleted"] != true
                })
                .collect::<Vec<_>>();
            ensure!(channels.len() == 1, "没有唯一可用的数字人民币钱包支付渠道");
            Ok::<_, anyhow::Error>((codes, scalar(channels[0], "id")?))
        }
        .await;
        let (codes, channel) = match prepared {
            Ok(prepared) => prepared,
            Err(error) => {
                // Do not surface upstream messages, which may contain wallet/account details.
                let message = error.to_string();
                payment.account_attempt = Some(if message.starts_with("校园付拒绝请求") {
                    "本次未扣款：校园付未能提供钱包渠道，请到校园付收银台付款".into()
                } else {
                    format!("本次未扣款：{message}")
                });
                return Ok(payment);
            }
        };
        self.claim_attempt(cashier)?;
        let transaction_id = cashier_id(cashier)?;
        let mut attempts = Vec::new();
        for (position, code) in codes {
            let bank = BANKS
                .iter()
                .find(|(id, _)| *id == code)
                .map(|(_, name)| *name)
                .unwrap_or("已绑定钱包");
            let label = format!("第{position}个钱包（{bank}）");
            payment.automatic = Some(AutomaticPayment {
                outcome: "uncertain".into(),
                debit_submitted: true,
            });
            // One dispatch for this wallet. Never replay on transport errors.
            let result = self
                .cashier_response(
                    "/transaction/pay",
                    &fields(&[
                        ("id", &transaction_id),
                        ("payWayId", &channel),
                        ("phoneNumber", ""),
                        ("ecCode", &code),
                    ]),
                )
                .await;
            let rejected = result
                .as_ref()
                .is_ok_and(|v| definitely_rejected(v, &transaction_id));
            let mut verified_rejection = false;
            for check in 0..if rejected { 1 } else { 3 } {
                if check > 0 {
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                }
                match self.transaction(cashier).await {
                    Ok(tx)
                        if same_transaction(&tx, cashier, Some(&payment.amount)).is_ok()
                            && !val(&tx, "status").is_empty() =>
                    {
                        apply_transaction(&mut payment, &tx);
                        if payment.status == "success" {
                            attempts.push(format!("{label}：校园付已确认付款成功"));
                            payment.account_attempt = Some(attempts.join("；"));
                            return Ok(payment);
                        }
                        if payment.status != "wait_payer_pay" {
                            payment.account_attempt = Some(format!(
                                "{label}付款后，校园付订单状态为 {}；已停止后续钱包尝试",
                                payment.status
                            ));
                            return Ok(payment);
                        }
                        if rejected {
                            verified_rejection = true;
                            break;
                        }
                    }
                    _ => break,
                }
            }
            if !verified_rejection {
                attempts.push(format!("{label}：结果待确认，已停止后续钱包尝试，请查询付款状态并核对数字人民币 App 或商户订单"));
                payment.account_attempt = Some(attempts.join("；"));
                return Ok(payment);
            }
            attempts.push(format!("{label}：明确付款失败"));
            payment.automatic = Some(AutomaticPayment {
                outcome: "rejected".into(),
                debit_submitted: true,
            });
        }
        attempts.push(
            if preference == 0 {
                "所有已绑定钱包已按顺序尝试一遍，支付失败"
            } else {
                "所选钱包支付失败，未尝试其他钱包"
            }
            .into(),
        );
        payment.account_attempt = Some(attempts.join("；"));
        Ok(payment)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn wallet_order_is_preserved_and_explicit_index_selects_only_one() {
        let ways = json!({"ecCode":["icbc", "ccb", "abc"]});
        assert_eq!(
            wallet_codes(&ways, 0).unwrap(),
            vec![(1, "icbc".into()), (2, "ccb".into()), (3, "abc".into())]
        );
        assert_eq!(wallet_codes(&ways, 2).unwrap(), vec![(2, "ccb".into())]);
        assert!(wallet_codes(&ways, 4).is_err());
        assert!(wallet_codes(&json!({"ecCode":[]}), 0).is_err());
        assert!(wallet_codes(&json!({"ecCode":["icbc","icbc"]}), 0).is_err());
    }
    #[test]
    fn timeout_and_unknown_rejections_never_rotate_wallets() {
        assert!(definitely_rejected(
            &json!({"success":false,"message":"钱包余额不足"}),
            "T"
        ));
        assert!(definitely_rejected(
            &json!({"success":false,"data":{"isPaid":false}}),
            "T"
        ));
        for value in [
            json!({"success":false,"message":"请求失败"}),
            json!({"success":false,"message":"超时","data":{"isPaid":false}}),
            json!({"success":true,"data":{"isPaid":false}}),
            json!({"success":false,"message":"余额不足","data":{"isPaid":true}}),
        ] {
            assert!(!definitely_rejected(&value, "T"));
        }
    }
    #[test]
    fn debit_amounts_are_exact_and_currency_is_checked() {
        assert_eq!(cents("1.01"), Some(101));
        for value in ["-1", "NaN", "1.001", "1e3", " 1", "1.2.3"] {
            assert!(cents(value).is_none());
        }
        let tx = json!({"currency":"USD","money":"1.00"});
        assert!(same_transaction(&tx, "https://cashier.cc-pay.cn/cashier?id=T", None).is_err());
    }
}
