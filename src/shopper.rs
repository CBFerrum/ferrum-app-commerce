//! What a shopper does after buying, from the signed link in their order confirmation (the platform's
//! action links: a link names one record's actions and a guest bound to it runs them): cancel the order
//! while it has not shipped, or ask to send goods back once they were delivered. Each is an action of the
//! order, so the link's own checks hold and the order's history names the link; the store's windows and
//! terms are Store Settings'. A cancellation is refunded as any is (`refund::cancel_order`); a return is a
//! `ReturnRequest` the desk approves or declines, and once the goods are in, a refund of the order's
//! own (which credits the invoice, restocks and goes back to the payment or as store credit).

use crate::payment::param_text;
use crate::refund::{claims, reason_named};
use crate::store::{Store, notify_managers};
use crate::tell;
use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use ferrum_domain::util::{b, d, get, id, money, rows, s, set, sys};
use ferrum_kernel::action_link::LinkSpec;
use ferrum_kernel::{Ctx, Doc, DocHook, Error, HookCtx, Kernel, Result, Tx};
use rust_decimal::Decimal;
use serde_json::{Map, Value, json};
use uuid::Uuid;

/// The link in an order confirmation: the actions the store lets shoppers take, on this order, for as long
/// as the longest of its windows (90 days at most). `None` when the store lets them none, or has no
/// secret key to keep the token sealed while its mail waits (it is never sent in the clear). The address
/// comes in two parts, its start and its token, because a mail's HTML escapes what it prints (the slashes of
/// an address) and a secret value is masked only where it is printed as it is: the token alone is.
pub async fn manage_link(k: &Kernel, ctx: &Ctx, tx: &mut Tx, store: &Store, so: Uuid, email: Option<&str>) -> Option<(String, String)> {
    let p = &store.policy;
    let mut actions = Vec::new();
    if p.cancel_hours > 0 {
        actions.push("web_shopper_cancel".to_string());
    }
    if p.return_days > 0 {
        actions.push("web_shopper_return".to_string());
    }
    if actions.is_empty()
        || k.settings.secret_key.is_none()
        || ctx.schema.entity("SalesOrder").is_none_or(|e| e.def.actions.iter().all(|a| a.name != "web_shopper_cancel"))
    {
        return None;
    }
    let days = (p.cancel_hours / 24 + 1).max(p.return_days + 30).min(90);
    let spec = LinkSpec {
        actions,
        show: ["posting_date", "commerce_fulfilment_status", "rounded_total"].map(String::from).to_vec(),
        params: Map::new(),
        expires_at: Some(Utc::now() + Duration::days(days)),
        uses: Some(10),
        code_to: None,
        recipient: email.map(str::to_string),
        frame_origins: vec![],
    };
    match k.issue_link(&sys(ctx), tx, "SalesOrder", so, &spec).await {
        Ok(issued) => issued.url.rsplit_once('/').map(|(start, token)| (format!("{start}/"), token.to_string())),
        Err(e) => {
            tracing::warn!(%so, "no link to manage the order was made: {e}");
            None
        }
    }
}

fn guard(so: &Doc) -> Result<()> {
    if !b(so, "commerce_web_order") {
        return Err(Error::invalid("this is not a web order"));
    }
    if d(so, "docstatus") != Decimal::ONE {
        return Err(Error::invalid("this order was cancelled"));
    }
    Ok(())
}

fn at(so: &Doc, f: &str) -> Option<DateTime<Utc>> {
    s(so, f).and_then(|t| ferrum_kernel::value::parse_datetime(t).ok())
}

/// "Cancel My Order": within the store's window, before anything shipped.
pub async fn cancel(hc: &mut HookCtx<'_>, so: &Doc, params: &Value) -> Result<Value> {
    guard(so)?;
    let sctx = sys(hc.ctx);
    let store = Store::load(hc.kernel, &sctx, hc.tx).await?;
    let hours = store.policy.cancel_hours;
    if hours == 0 {
        return Err(Error::invalid("this store does not take cancellations from shoppers: write to the store"));
    }
    if at(so, "created_at").is_some_and(|placed| placed + Duration::hours(hours) < Utc::now()) {
        return Err(Error::invalid(format!("the {hours} hours in which you could cancel this order yourself have passed: write to the store")));
    }
    let so_id = id(so, "id").ok_or_else(|| Error::internal("order without id"))?;
    let why = param_text(params, "reason", 2000)?;
    let out = crate::refund::cancel_order(hc, so, None, why, store.policy.cancel_auto).await?;
    let after = get(hc.kernel, &sctx, hc.tx, "SalesOrder", so_id).await?;
    let mut values = Map::new();
    values.insert("refund_waits".into(), json!(!store.policy.cancel_auto && out["refunds"].as_array().is_some_and(|r| !r.is_empty())));
    values.insert(
        "link".into(),
        json!(tell::order_link(hc.tx, hc.ctx.tenant, store.store_url.as_deref(), s(&after, "number").unwrap_or_default(), so_id).await?),
    );
    tell::message(hc.kernel, hc.tx, &sctx, "commerce.order_cancelled", so_id, s(&after, "commerce_contact_email"), values, vec![]).await?;
    notify_managers(hc.tx, hc.ctx.tenant, "A shopper cancelled an order", s(&after, "number"), "SalesOrder", so_id).await?;
    Ok(out)
}

/// What the text of lines asks for: `1x2, 3` is line 1 twice and line 3 once; empty: nothing named.
pub(crate) fn parse_lines(text: &str) -> Result<Vec<(usize, Option<Decimal>)>> {
    let bad = |t: &str| Error::field("lines", format!("\"{t}\" is not a line number, or a line number and how many, as 1x2"));
    let mut out = Vec::new();
    for part in text.split([',', ';', '\n']).map(str::trim).filter(|p| !p.is_empty()) {
        let (line, qty) = match part.split_once(['x', 'X', '×', '*']) {
            Some((l, q)) => (l.trim(), Some(q.trim().parse::<Decimal>().ok().filter(|q| *q > Decimal::ZERO).ok_or_else(|| bad(part))?)),
            None => (part, None),
        };
        let n: usize = line.parse().ok().filter(|n| *n >= 1).ok_or_else(|| bad(part))?;
        if out.iter().any(|(l, _)| *l == n) {
            return Err(Error::field("lines", format!("line {n} is listed twice")));
        }
        out.push((n, qty));
    }
    Ok(out)
}

/// Units of each order line that open return requests already ask for.
async fn asked_for(tx: &mut Tx, tenant: Uuid, so: Uuid) -> Result<std::collections::HashMap<String, Decimal>> {
    let found: Vec<(String, Decimal)> = sqlx::query_as(
        "select l.sales_order_line, sum(l.qty) from e_return_request__lines l join e_return_request r on r.tenant_id = l.tenant_id and r.id = l.parent_id \
         where r.tenant_id = $1 and r.sales_order = $2 and r.status in ('Requested', 'Approved') group by l.sales_order_line",
    )
    .bind(tenant)
    .bind(so)
    .fetch_all(&mut **tx)
    .await?;
    Ok(found.into_iter().collect())
}

/// The reason a shopper's words match (any case), else "Other".
async fn reason_for(hc: &mut HookCtx<'_>, text: Option<&str>) -> Result<Uuid> {
    if let Some(t) = text {
        let found: Option<Uuid> =
            sqlx::query_scalar("select id from e_return_reason where tenant_id = $1 and lower(reason) = lower($2) and not coalesce(disabled, false)")
                .bind(hc.ctx.tenant)
                .bind(t)
                .fetch_optional(&mut **hc.tx)
                .await?;
        if let Some(r) = found {
            return Ok(r);
        }
    }
    reason_named(hc, "Other").await
}

/// "Return Something": a request the desk will decide, for the lines and quantities chosen (everything
/// delivered when none are), within the store's days after the last delivery.
pub async fn request_return(hc: &mut HookCtx<'_>, so: &Doc, params: &Value) -> Result<Value> {
    guard(so)?;
    let sctx = sys(hc.ctx);
    let so_id = id(so, "id").ok_or_else(|| Error::internal("order without id"))?;
    let store = Store::load(hc.kernel, &sctx, hc.tx).await?;
    let days = store.policy.return_days;
    if days == 0 {
        return Err(Error::invalid("this store does not take returns from shoppers: write to the store"));
    }
    if d(so, "per_delivered") <= Decimal::ZERO {
        return Err(Error::invalid("nothing on this order has been delivered yet: cancel it instead"));
    }
    let delivered_at: Option<DateTime<Utc>> = sqlx::query_scalar(
        "select max(n.created_at) from e_delivery_note n where n.tenant_id = $1 and n.docstatus = 1 and not coalesce(n.is_return, false) \
         and exists (select 1 from e_delivery_note__items i where i.tenant_id = n.tenant_id and i.parent_id = n.id and i.sales_order = $2)",
    )
    .bind(hc.ctx.tenant)
    .bind(so_id)
    .fetch_one(&mut **hc.tx)
    .await?;
    if delivered_at.is_some_and(|t| t + Duration::days(days) < Utc::now()) {
        return Err(Error::invalid(format!("the {days} days in which goods could be sent back have passed: write to the store")));
    }
    let lines = rows(so, "items");
    let claimed = claims(hc.tx, hc.ctx.tenant, so_id, None).await?;
    let open = asked_for(hc.tx, hc.ctx.tenant, so_id).await?;
    let room = |l: &Doc| {
        let key = s(l, "id").unwrap_or_default();
        let c = claimed.get(key).copied().unwrap_or_default();
        (d(l, "delivered_qty") + c.restocked - c.all - open.get(key).copied().unwrap_or_default()).max(Decimal::ZERO)
    };
    // Lines are numbered as the shopper sees them: the goods, in order, without the delivery charge.
    let is_shipping = |l: &Doc| store.shipping_item.is_some() && id(l, "item") == store.shipping_item;
    let lines: Vec<Doc> = lines.into_iter().filter(|l| !is_shipping(l)).collect();
    let named = parse_lines(&param_text(params, "lines", 1000)?.unwrap_or_default())?;
    let chosen: Vec<(usize, Decimal)> = if named.is_empty() {
        lines.iter().enumerate().map(|(i, l)| (i, room(l))).filter(|(_, q)| *q > Decimal::ZERO).collect()
    } else {
        let mut v = Vec::new();
        for (n, qty) in named {
            let l = lines.get(n - 1).ok_or_else(|| Error::field("lines", format!("this order has no item {n}")))?;
            let name = s(l, "item_name").unwrap_or("this item");
            let q = qty.unwrap_or_else(|| room(l));
            if q > room(l) {
                return Err(Error::field("lines", format!("at most {} of {name} can still be sent back", room(l).normalize())));
            }
            v.push((n - 1, q));
        }
        v
    };
    if chosen.is_empty() {
        return Err(Error::invalid("everything delivered on this order has been sent back or asked for already"));
    }
    let reason = reason_for(hc, param_text(params, "reason", 140)?.as_deref()).await?;
    let table: Vec<Value> = chosen
        .iter()
        .map(
            |(i, q)| json!({"sales_order_line": s(&lines[*i], "id"), "item": s(&lines[*i], "item"), "item_name": s(&lines[*i], "item_name"), "qty": money(*q)}),
        )
        .collect();
    let p = &store.policy;
    let request = json!({
        "company": s(so, "company"), "sales_order": so_id.to_string(), "order_number": s(so, "number"), "customer": s(so, "customer"),
        "email": s(so, "commerce_contact_email"), "status": "Requested", "reason": reason.to_string(), "shopper_note": param_text(params, "note", 2000)?,
        "lines": table, "resolution": if p.credit_by_default && p.credit_kind.is_some() { "Store Credit" } else { "Original Payment" },
        "shipping_paid_by": if p.store_pays_return_shipping { "Store" } else { "Shopper" }, "restocking_fee_percent": money(p.restocking_fee),
        "requested_at": crate::now_str(),
    });
    let made = hc.kernel.insert_tx(&sctx, hc.tx, "ReturnRequest", request.as_object().cloned().unwrap_or_default()).await?;
    let rid = id(&made, "id").ok_or_else(|| Error::internal("request without id"))?;
    notify_managers(hc.tx, hc.ctx.tenant, "A shopper asks to return goods", s(so, "number"), "ReturnRequest", rid).await?;
    let mut values = Map::new();
    values.insert("number".into(), json!(s(&made, "number")));
    values.insert(
        "lines".into(),
        json!(chosen.iter().map(|(i, q)| format!("{} × {}", q.normalize(), s(&lines[*i], "item_name").unwrap_or_default())).collect::<Vec<_>>()),
    );
    tell::message(hc.kernel, hc.tx, &sctx, "commerce.return_requested", so_id, s(so, "commerce_contact_email"), values, vec![]).await?;
    Ok(json!({"return_request": rid.to_string(), "number": s(&made, "number")}))
}

pub struct ReturnRequestHook;

fn person(hc: &HookCtx<'_>) -> Result<()> {
    if hc.ctx.outside || hc.ctx.email.ends_with(crate::payment::PLUGIN_DOMAIN) {
        return Err(Error::Forbidden("a person at the store decides this".into()));
    }
    Ok(())
}

#[async_trait]
impl DocHook for ReturnRequestHook {
    async fn action(&self, hc: &mut HookCtx<'_>, name: &str, doc: &Doc, params: &Value) -> Result<Option<Value>> {
        if !matches!(name, "approve_return" | "decline_return" | "receive_return") {
            return Ok(None);
        }
        person(hc)?;
        let sctx = sys(hc.ctx);
        let rid = id(doc, "id").ok_or_else(|| Error::internal("request without id"))?;
        let status = s(doc, "status").unwrap_or_default().to_string();
        let so_id = id(doc, "sales_order").ok_or_else(|| Error::internal("request without order"))?;
        let note = param_text(params, "note", 2000)?;
        let to = s(doc, "email").map(str::to_string);
        let mut v = Doc::new();
        Ok(Some(match name {
            "approve_return" | "decline_return" => {
                if status != "Requested" {
                    return Err(Error::Conflict(format!("this request is {status}")));
                }
                let approve = name == "approve_return";
                if !approve && note.is_none() {
                    return Err(Error::field("note", "say why it is declined: the shopper is told"));
                }
                set(&mut v, "status", if approve { "Approved" } else { "Declined" });
                set(&mut v, "decision_note", json!(note));
                set(&mut v, "decided_by", hc.ctx.email.clone());
                hc.kernel.set_values_tx(&sctx, hc.tx, "ReturnRequest", rid, v).await?;
                let mut values = Map::new();
                values.insert("note".into(), json!(note));
                values.insert("store_pays_shipping".into(), json!(s(doc, "shipping_paid_by") == Some("Store")));
                values.insert(
                    "fee".into(),
                    json!((d(doc, "restocking_fee_percent") > Decimal::ZERO).then(|| d(doc, "restocking_fee_percent").normalize().to_string())),
                );
                values.insert("number".into(), json!(s(doc, "number")));
                let template = if approve { "commerce.return_approved" } else { "commerce.return_declined" };
                tell::message(hc.kernel, hc.tx, &sctx, template, so_id, to.as_deref(), values, vec![]).await?;
                json!({"status": if approve { "Approved" } else { "Declined" }})
            }
            _ => {
                if status != "Approved" {
                    return Err(Error::Conflict(format!("this request is {status}")));
                }
                let reason = id(doc, "reason").ok_or_else(|| Error::internal("request without reason"))?;
                let restock = b(&get(hc.kernel, &sctx, hc.tx, "ReturnReason", reason).await?, "restock");
                let lines: Vec<Value> = rows(doc, "lines")
                    .iter()
                    .map(|l| json!({"sales_order_line": s(l, "sales_order_line"), "qty": money(d(l, "qty")), "restock": restock}))
                    .collect();
                let refund = json!({
                    "sales_order": so_id.to_string(), "kind": "Return", "reason": reason.to_string(), "lines": lines, "notify_customer": true,
                    "destination": s(doc, "resolution"), "restocking_fee_percent": money(d(doc, "restocking_fee_percent")), "return_request": rid.to_string(),
                });
                // Made with the system's rights (the terms the request carries are read-only fields), asked for by the person.
                let made = hc.kernel.insert_tx(&sctx, hc.tx, "OrderRefund", refund.as_object().cloned().unwrap_or_default()).await?;
                let refund = id(&made, "id").ok_or_else(|| Error::internal("refund without id"))?;
                let mut by = Doc::new();
                set(&mut by, "requested_by", hc.ctx.email.clone());
                hc.kernel.set_values_tx(&sctx, hc.tx, "OrderRefund", refund, by).await?;
                set(&mut v, "status", "Received");
                set(&mut v, "refund", refund.to_string());
                hc.kernel.set_values_tx(&sctx, hc.tx, "ReturnRequest", rid, v).await?;
                json!({"status": "Received", "refund": refund.to_string(), "amount": money(d(&made, "amount"))})
            }
        }))
    }
}

/// A return request is refunded when its refund has gone back.
pub async fn refunded(k: &Kernel, ctx: &Ctx, tx: &mut Tx, request: Uuid) -> Result<()> {
    let mut v = Doc::new();
    set(&mut v, "status", "Refunded");
    k.set_values_tx(&sys(ctx), tx, "ReturnRequest", request, v).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_shopper_names_lines_and_how_many() {
        let n = |t: &str| t.parse::<Decimal>().unwrap();
        assert_eq!(parse_lines("").unwrap(), vec![]);
        assert_eq!(parse_lines("1x2, 3").unwrap(), vec![(1, Some(n("2"))), (3, None)]);
        assert_eq!(parse_lines(" 2 × 1.5 ;\n4").unwrap(), vec![(2, Some(n("1.5"))), (4, None)]);
        for bad in ["a", "0", "1x0", "1x", "1,1", "-1"] {
            assert!(parse_lines(bad).is_err(), "{bad}");
        }
    }
}
