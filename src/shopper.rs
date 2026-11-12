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
    // As long as the windows that apply: a cancellation's, and a return's from a delivery that may be a month
    // away. A store with no returns has no link beyond its cancellation window.
    let cancel_days = if p.cancel_hours > 0 { p.cancel_hours / 24 + 1 } else { 0 };
    let return_span = if p.return_days > 0 { p.return_days + 30 } else { 0 };
    let days = cancel_days.max(return_span).clamp(1, 90);
    let mut show: Vec<String> = ["posting_date", "commerce_fulfilment_status", "rounded_total", "items"].map(String::from).to_vec();
    // A workspace not yet updated has no field to keep the terms in: its page shows the order alone.
    let has_field = ctx.schema.entity("SalesOrder").is_some_and(|e| e.field("commerce_after_sales_terms").is_some());
    let terms = terms_text(p).filter(|_| has_field);
    if let Some(words) = &terms {
        let mut v = Doc::new();
        set(&mut v, "commerce_after_sales_terms", words.clone());
        k.set_values_tx(&sys(ctx), tx, "SalesOrder", so, v).await.ok()?;
        show.push("commerce_after_sales_terms".into());
    }
    let spec = LinkSpec {
        actions,
        show,
        params: Map::new(),
        expires_at: Some(Utc::now() + Duration::days(days)),
        uses: Some(10),
        code_to: None,
        recipient: email.map(str::to_string),
        frame_origins: vec![],
        // What was bought, line by line, beside the date, status and total.
        columns: std::collections::BTreeMap::from([("items".to_string(), ["item_name", "qty", "rate", "amount"].map(String::from).to_vec())]),
    };
    match k.issue_link(&sys(ctx), tx, "SalesOrder", so, &spec).await {
        Ok(issued) => issued.url.rsplit_once('/').map(|(start, token)| (format!("{start}/"), token.to_string())),
        Err(e) => {
            tracing::warn!(%so, "no link to manage the order was made: {e}");
            None
        }
    }
}

/// The workspace's words about the store's terms, with the settings they name filled in: `{hours}`, `{days}`,
/// `{fee}`, `{shipping}` (who pays it) and `{refund}` (where it goes). `None` when the workspace says nothing.
pub(crate) fn terms_text(p: &crate::store::Policy) -> Option<String> {
    let words = p.page_text.as_deref()?.trim();
    if words.is_empty() {
        return None;
    }
    Some(
        words
            .replace("{hours}", &p.cancel_hours.to_string())
            .replace("{days}", &p.return_days.to_string())
            .replace("{fee}", &p.restocking_fee.normalize().to_string())
            .replace("{shipping}", if p.store_pays_return_shipping { "Store" } else { "Shopper" })
            .replace("{refund}", if p.credit_by_default { "Store Credit" } else { "Original Payment" }),
    )
}

/// What a refund goes back to, as the shopper is told: each payment's method, the card's first.
fn refund_labels(store: &Store, refunds: &Value) -> (Decimal, Vec<String>) {
    let mut total = Decimal::ZERO;
    let mut to: Vec<String> = Vec::new();
    for r in refunds.as_array().into_iter().flatten() {
        total += r.get("amount").and_then(Value::as_str).and_then(|a| a.parse::<Decimal>().ok()).unwrap_or_default();
        let gateway = r.get("gateway").and_then(Value::as_str).unwrap_or_default();
        let label = store.method(gateway).map_or_else(|| gateway.to_string(), |m| m.label.clone());
        if !label.is_empty() && !to.contains(&label) {
            to.push(label);
        }
    }
    (total, to)
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
    let (back, to) = refund_labels(&store, &out["refunds"]);
    values.insert("refund_amount".into(), json!((back > Decimal::ZERO).then(|| crate::pages::money(&store.currency_code, &back.to_string(), store.precision))));
    values.insert("refund_to".into(), json!((!to.is_empty()).then(|| to.join(", "))));
    values.insert(
        "link".into(),
        json!(tell::order_link(hc.tx, hc.ctx.tenant, store.store_url.as_deref(), s(&after, "number").unwrap_or_default(), so_id).await?),
    );
    tell::message(hc.kernel, hc.tx, &sctx, "commerce.order_cancelled", so_id, s(&after, "commerce_contact_email"), values, vec![]).await?;
    notify_managers(hc.tx, hc.ctx.tenant, "A shopper cancelled an order", s(&after, "number"), "SalesOrder", so_id).await?;
    Ok(out)
}

/// One item a shopper names: by its number in the list or by its name.
#[derive(Debug, PartialEq)]
pub(crate) enum Pick {
    Number(usize),
    Name(String),
}

/// What the text of lines asks for: `Blue Mug x2, 3` is the Blue Mug twice and item 3 whole; empty: nothing named.
pub(crate) fn parse_lines(text: &str) -> Result<Vec<(Pick, Option<Decimal>)>> {
    let bad = |t: &str| Error::field("lines", format!("\"{t}\" is not an item's name or number, or one with how many, as Blue Mug x2"));
    let how_many = |t: &str| t.trim().parse::<Decimal>().ok().filter(|q| *q > Decimal::ZERO);
    let mut out = Vec::new();
    for part in text.split([',', ';', '\n']).map(str::trim).filter(|p| !p.is_empty()) {
        let (name, qty) = match part.rsplit_once(['x', 'X', '×', '*']) {
            Some((l, q)) if !l.trim().is_empty() && how_many(q).is_some() => (l.trim(), how_many(q)),
            Some((l, _)) if !l.trim().is_empty() && l.trim().bytes().all(|c| c.is_ascii_digit()) => return Err(bad(part)),
            _ => (part, None),
        };
        // Digits alone are a number (and then not zero); a sign is a mistake; anything else is a name.
        let pick = if name.bytes().all(|c| c.is_ascii_digit()) {
            Pick::Number(name.parse().ok().filter(|n| *n >= 1).ok_or_else(|| bad(part))?)
        } else if name.starts_with(['-', '+']) {
            return Err(bad(part));
        } else {
            Pick::Name(name.to_string())
        };
        if out.iter().any(|(p, _)| *p == pick) {
            return Err(Error::field("lines", format!("\"{name}\" is listed twice")));
        }
        out.push((pick, qty));
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
        let mut v: Vec<(usize, Decimal)> = Vec::new();
        for (pick, qty) in named {
            let at = match &pick {
                Pick::Number(n) => (*n <= lines.len()).then_some(*n - 1).ok_or_else(|| Error::field("lines", format!("this order has no item {n}")))?,
                Pick::Name(name) => {
                    let hits: Vec<usize> = (0..lines.len()).filter(|i| s(&lines[*i], "item_name").is_some_and(|x| x.eq_ignore_ascii_case(name))).collect();
                    match hits.as_slice() {
                        [one] => *one,
                        [] => {
                            return Err(Error::field(
                                "lines",
                                format!("this order has no item called {name}: write its name as the list has it, or its number"),
                            ));
                        }
                        _ => return Err(Error::field("lines", format!("more than one item is called {name}: write its number"))),
                    }
                }
            };
            if v.iter().any(|(i, _)| *i == at) {
                return Err(Error::field("lines", "an item is listed twice"));
            }
            let l = &lines[at];
            let name = s(l, "item_name").unwrap_or("this item");
            let q = qty.unwrap_or_else(|| room(l));
            if q > room(l) {
                return Err(Error::field("lines", format!("at most {} of {name} can still be sent back", room(l).normalize())));
            }
            v.push((at, q));
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
    values.insert("to_credit".into(), json!(p.credit_by_default && p.credit_kind.is_some()));
    values.insert("fee".into(), json!((p.restocking_fee > Decimal::ZERO).then(|| p.restocking_fee.normalize().to_string())));
    values.insert("store_pays_shipping".into(), json!(p.store_pays_return_shipping));
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
        let name = |t: &str| Pick::Name(t.to_string());
        assert_eq!(parse_lines("").unwrap(), vec![]);
        assert_eq!(parse_lines("1x2, 3").unwrap(), vec![(Pick::Number(1), Some(n("2"))), (Pick::Number(3), None)]);
        assert_eq!(parse_lines(" 2 × 1.5 ;\n4").unwrap(), vec![(Pick::Number(2), Some(n("1.5"))), (Pick::Number(4), None)]);
        assert_eq!(
            parse_lines("Blue Mug x2, Box, logo tee X 1").unwrap(),
            vec![(name("Blue Mug"), Some(n("2"))), (name("Box"), None), (name("logo tee"), Some(n("1")))]
        );
        for bad in ["0", "1x0", "1x", "1,1", "-1", "Mug,Mug"] {
            assert!(parse_lines(bad).is_err(), "{bad}");
        }
    }
}
