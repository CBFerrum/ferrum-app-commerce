//! A mail to the shopper sent again by the desk ("Send Mail Again"): one of the shopper mails whose values
//! are what the order itself says (a shipment, a payment, a failed payment, a refund), told again as the order
//! stands, to the order's own address and no other. Counted on the order, and limited: the same mail goes to one
//! order only so many times in a day (Store Settings). A mail that holds a secret value (a gift card's code, the
//! order confirmation's link, store credit's code) is never one of them: its value is not kept anywhere to tell again.

use crate::store::Store;
use crate::tell;
use ferrum_domain::util::{d, find, id, money, s, set, sys};
use ferrum_kernel::messages::{self, Message, Recipient};
use ferrum_kernel::{Doc, Error, Filter, HookCtx, Result};
use rust_decimal::Decimal;
use serde_json::{Map, Value, json};

/// The desk's choice and the template it sends again.
const MAILS: [(&str, &str); 4] = [
    ("Shipped", "commerce.order_shipped"),
    ("Payment Received", "commerce.payment_received"),
    ("Payment Failed", "commerce.payment_failed"),
    ("Refunded", "commerce.order_refunded"),
];

/// The values of the mail as the order stands now.
async fn values(hc: &mut HookCtx<'_>, store: &Store, so: &Doc, template: &str) -> Result<Map<String, Value>> {
    let sctx = sys(hc.ctx);
    let so_id = id(so, "id").ok_or_else(|| Error::internal("order without id"))?;
    let number = s(so, "number").unwrap_or_default();
    let mut v = Map::new();
    v.insert("link".into(), json!(tell::order_link(hc.tx, hc.ctx.tenant, store.store_url.as_deref(), number, so_id).await?));
    let money_of = |code: &str, a: Decimal| crate::pages::money(code, &a.to_string(), store.precision);
    match template {
        "commerce.order_shipped" => {
            let notes = find(
                hc.kernel,
                &sctx,
                hc.tx,
                "DeliveryNote",
                vec![Filter::eq("items.sales_order", so_id.to_string()), Filter::eq("docstatus", 1), Filter::eq("is_return", false)],
                Some("created_at desc"),
                1,
            )
            .await?;
            let Some(note) = notes.first().and_then(|n| id(n, "id")) else { return Err(Error::invalid("nothing on this order has shipped")) };
            let note = hc.kernel.get_tx(&sctx, hc.tx, "DeliveryNote", note, false).await?;
            for (k, x) in crate::ship::shipped_values(&note, so_id) {
                v.insert(k.into(), x);
            }
        }
        "commerce.payment_received" | "commerce.payment_failed" => {
            let wanted = if template == "commerce.payment_received" { "Paid" } else { "Failed" };
            let found = find(
                hc.kernel,
                &sctx,
                hc.tx,
                "PaymentRequest",
                vec![Filter::eq("sales_order", so_id.to_string()), Filter::eq("status", wanted)],
                Some("created_at desc"),
                1,
            )
            .await?;
            let Some(pr) = found.first() else { return Err(Error::invalid(format!("this order has no {} payment to tell of", wanted.to_lowercase()))) };
            if wanted == "Paid" {
                let gateway = s(pr, "gateway").unwrap_or_default();
                v.insert("amount".into(), json!(money_of(s(pr, "currency_code").unwrap_or_default(), d(pr, "amount"))));
                v.insert("method".into(), json!(store.method(gateway).map(|m| m.label.clone()).unwrap_or_else(|| gateway.to_string())));
            } else {
                v.insert("reason".into(), json!(s(pr, "failure_reason")));
            }
        }
        _ => {
            let found = find(
                hc.kernel,
                &sctx,
                hc.tx,
                "OrderRefund",
                vec![Filter::eq("sales_order", so_id.to_string()), Filter::eq("status", "Refunded")],
                Some("created_at desc"),
                1,
            )
            .await?;
            let Some(r) = found.first() else { return Err(Error::invalid("nothing has been sent back on this order yet")) };
            v.insert("amount".into(), json!(money_of(s(r, "currency_code").unwrap_or_default(), d(r, "amount"))));
        }
    }
    Ok(v)
}

/// "Send Mail Again".
pub async fn web_resend_mail(hc: &mut HookCtx<'_>, so: &Doc, params: &Value) -> Result<Value> {
    crate::ship::guard(hc, so)?;
    let sctx = sys(hc.ctx);
    let so_id = id(so, "id").ok_or_else(|| Error::internal("order without id"))?;
    let chosen = crate::payment::param_text(params, "mail", 60)?.ok_or_else(|| Error::field("mail", "choose the mail to send again"))?;
    let template = MAILS
        .iter()
        .find(|(label, _)| label.eq_ignore_ascii_case(chosen.trim()))
        .map(|(_, key)| *key)
        .ok_or_else(|| Error::field("mail", "this mail cannot be sent again: it holds a secret value, or tells what only the moment had"))?;
    let to = s(so, "commerce_contact_email").ok_or_else(|| Error::invalid("this order has no email address to write to"))?.to_string();
    let store = Store::load(hc.kernel, &sctx, hc.tx).await?;
    // The same mail to the same order only so often: sends of it in the last day, the first one among them.
    let day_ago = crate::now() - chrono::Duration::hours(24);
    let log = hc.kernel.message_log(&sctx, Some(("SalesOrder", so_id)), None, 500).await?;
    let lately = log
        .iter()
        .filter(|m| m["template"] == json!(template))
        .filter(|m| m["created_at"].as_str().and_then(|t| ferrum_kernel::value::parse_datetime(t).ok()).is_some_and(|t| t > day_ago))
        .count() as i64;
    if lately >= store.resend_per_day {
        return Err(Error::Conflict(format!("this mail has gone to this order {lately} times today: the limit is {}", store.resend_per_day)));
    }
    let message = Message {
        template: template.into(),
        entity: Some("SalesOrder".into()),
        record: Some(so_id),
        to: vec![Recipient::address(&to)],
        values: values(hc, &store, so, template).await?,
        ..Default::default()
    };
    let sent = messages::send(hc.kernel, hc.tx, &sctx, &message).await?;
    let times = d(so, "commerce_mails_resent") + Decimal::ONE;
    let mut v = Doc::new();
    set(&mut v, "commerce_mails_resent", money(times));
    hc.kernel.set_values_tx(&sctx, hc.tx, "SalesOrder", so_id, v).await?;
    let state = sent.first().map(|x| x.state.clone());
    Ok(
        json!({"sent": state.as_deref() == Some("queued") || state.as_deref() == Some("sent"), "state": state, "mail": chosen, "resent": times.normalize().to_string(),
              "left_today": store.resend_per_day - lately - 1}),
    )
}
