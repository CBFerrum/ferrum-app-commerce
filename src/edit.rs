//! Changing a web order after it was placed and before it is fulfilled (the desk's "Update Web Order").
//!
//! The lines go through ERP's Update Items (which refuses what has shipped or been billed, and prices
//! promotions again); the address is the order's own, so tax follows where it now goes. What the order
//! then comes to, against what the shopper has paid, is settled through the order's own payment where it
//! can be: an order still unpaid has its open request replaced; a paid one is asked for the difference
//! through the method it was paid with, or sent the difference back as an Adjustment refund (a person
//! approves it and the gateway carries it out, as any refund). Where neither can be done the amount is
//! recorded as owed, either way, on the order, and the shopper told.

use crate::cart::address_text;
use crate::gift_card::GIFT_CARD;
use crate::payment::{open_request, param_text};
use crate::refund::{for_person, reason_named};
use crate::shipping::{Place, Rule, enabled_rules};
use crate::store::Store;
use crate::tell;
use ferrum_domain::util::{d, elevated, find, get, id, money, rows, s, set, sys};
use ferrum_kernel::{Action, Ctx, Doc, Error, Filter, HookCtx, Kernel, Result};
use rust_decimal::Decimal;
use serde_json::{Map, Value, json};
use uuid::Uuid;

/// One change the desk asks for: an existing line (by its number on the order) to a new quantity (0
/// removes it), or an item added.
#[derive(Debug, PartialEq)]
pub(crate) struct Change {
    line: Option<i64>,
    item: Option<Uuid>,
    qty: Decimal,
}

/// `changes` from the desk: a list (or its JSON text) of `{line | item, qty}`.
pub(crate) fn changes(params: &Value) -> Result<Vec<Change>> {
    let raw = match params.get("changes") {
        None | Some(Value::Null) => return Ok(vec![]),
        Some(Value::String(t)) if t.trim().is_empty() => return Ok(vec![]),
        Some(Value::String(t)) => serde_json::from_str::<Value>(t).map_err(|_| Error::field("changes", "is not a JSON list of changes"))?,
        Some(v) => v.clone(),
    };
    let Value::Array(list) = raw else { return Err(Error::field("changes", "must be a list of {line, qty} or {item, qty}")) };
    let mut out = Vec::new();
    for v in &list {
        let qty = v
            .get("qty")
            .and_then(|q| q.as_str().map(str::to_string).or_else(|| q.as_number().map(|n| n.to_string())))
            .and_then(|t| t.trim().parse::<Decimal>().ok())
            .filter(|q| *q >= Decimal::ZERO)
            .ok_or_else(|| Error::field("changes", "each change gives a quantity (0 removes the line)"))?;
        let line = v.get("line").and_then(|l| l.as_i64().or_else(|| l.as_str().and_then(|t| t.trim().parse().ok())));
        let item = match v.get("item").and_then(Value::as_str) {
            Some(i) => Some(i.parse().map_err(|_| Error::field("changes", "an item is named by its id"))?),
            None => None,
        };
        if line.is_some() == item.is_some() {
            return Err(Error::field("changes", "each change names a line to change or an item to add"));
        }
        if item.is_some() && qty.is_zero() {
            return Err(Error::field("changes", "add at least one of an item"));
        }
        out.push(Change { line, item, qty });
    }
    Ok(out)
}

/// `address` from the desk: `{line1, line2?, city?, state?, postal_code?, country}` as an object or JSON text.
fn address(params: &Value) -> Result<Option<Map<String, Value>>> {
    let raw = match params.get("address") {
        None | Some(Value::Null) => return Ok(None),
        Some(Value::String(t)) if t.trim().is_empty() => return Ok(None),
        Some(Value::String(t)) => serde_json::from_str::<Value>(t).map_err(|_| Error::field("address", "is not a JSON object"))?,
        Some(v) => v.clone(),
    };
    let Value::Object(m) = raw else { return Err(Error::field("address", "must be {line1, city, state, postal_code, country}")) };
    let text = |k: &str| m.get(k).and_then(Value::as_str).map(str::trim).filter(|v| !v.is_empty());
    if text("line1").is_none() || text("city").is_none() || text("country").is_none() {
        return Err(Error::field("address", "needs at least line1, city and country"));
    }
    Ok(Some(m))
}

/// What the order's payments leave it owing: its total less what was paid, plus what edits sent back.
/// Positive: the shopper owes it; negative: the store owes the shopper.
pub async fn owed(tx: &mut ferrum_kernel::Tx, tenant: Uuid, so: &Doc) -> Result<Decimal> {
    let so_id = id(so, "id").ok_or_else(|| Error::internal("order without id"))?;
    let paid: Decimal = sqlx::query_scalar(
        "select coalesce(sum(amount), 0) from e_payment_request where tenant_id = $1 and sales_order = $2 and status = 'Paid' and docstatus = 1",
    )
    .bind(tenant)
    .bind(so_id)
    .fetch_one(&mut **tx)
    .await?;
    let back: Decimal = sqlx::query_scalar(
        "select coalesce(sum(amount), 0) from e_order_refund where tenant_id = $1 and sales_order = $2 and kind = 'Adjustment' and status in ('Requested', 'Approved', 'Refund Failed', 'Refunded')",
    )
    .bind(tenant)
    .bind(so_id)
    .fetch_one(&mut **tx)
    .await?;
    Ok(d(so, "rounded_total") - paid + back)
}

/// Brings the order's recorded balance up to date after a payment or a refund, when edits have given it one.
pub async fn refresh_owed(k: &ferrum_kernel::Kernel, ctx: &ferrum_kernel::Ctx, tx: &mut ferrum_kernel::Tx, so_id: Uuid) -> Result<()> {
    if ctx.schema.entity("SalesOrder").is_none_or(|e| e.field("commerce_amount_owed").is_none()) {
        return Ok(());
    }
    let so = k.get_tx(&sys(ctx), tx, "SalesOrder", so_id, false).await?;
    if d(&so, "commerce_amount_owed").is_zero() {
        return Ok(());
    }
    let now = owed(tx, ctx.tenant, &so).await?;
    let mut v = Doc::new();
    set(&mut v, "commerce_amount_owed", money(now));
    k.set_values_tx(&sys(ctx), tx, "SalesOrder", so_id, v).await
}

/// "Update Web Order": the desk's change of lines and address on an order not yet fulfilled.
pub async fn web_update_items(hc: &mut HookCtx<'_>, so: &Doc, params: &Value) -> Result<Value> {
    crate::ship::guard(hc, so)?;
    let so_id = id(so, "id").ok_or_else(|| Error::internal("order without id"))?;
    if d(so, "docstatus") != Decimal::ONE {
        return Err(Error::invalid("this order was cancelled"));
    }
    if s(so, "status") == Some("Closed") || d(so, "per_delivered") >= Decimal::ONE_HUNDRED {
        return Err(Error::invalid(
            "everything on this order has been delivered or it is closed: its lines can no longer be changed, the goods can be sent back instead",
        ));
    }
    if d(so, "commerce_card_held") > Decimal::ZERO {
        return Err(Error::invalid(
            "a gift card holds part of this order until the rest is paid: wait for that payment, or cancel the order and place it again",
        ));
    }
    let asked = changes(params)?;
    let new_address = address(params)?;
    if asked.is_empty() && new_address.is_none() {
        return Err(Error::field("changes", "say what to change: lines, quantities or the address"));
    }
    let store = Store::load(hc.kernel, hc.ctx, hc.tx).await?;
    let actor = elevated(hc.ctx);
    let sctx = sys(hc.ctx);
    let before = d(so, "rounded_total");

    // What is added is for sale (published) and, when the store counts stock, in stock.
    for c in asked.iter().filter(|c| c.item.is_some()) {
        let on_sale: bool = sqlx::query_scalar("select exists(select 1 from e_website_item where tenant_id = $1 and item = $2 and published)")
            .bind(hc.ctx.tenant)
            .bind(c.item)
            .fetch_one(&mut **hc.tx)
            .await?;
        if !on_sale {
            return Err(Error::field("changes", "an item added must be one the store sells"));
        }
        if store.enforce_stock
            && let Some(item) = c.item
        {
            let stocked = crate::catalog::stock_items(hc.tx, hc.ctx.tenant, &[item]).await?;
            if stocked.contains(&item)
                && crate::catalog::available(hc.tx, hc.ctx.tenant, &store, &[item]).await?.get(&item).copied().unwrap_or_default() < c.qty
            {
                return Err(Error::field("changes", "there is not enough of an item added in stock"));
            }
        }
    }

    // Where it goes first, so that tax is worked out for the new place.
    if let Some(a) = &new_address {
        let text = |k: &str| a.get(k).and_then(Value::as_str).map(str::trim).filter(|v| !v.is_empty()).map(str::to_string);
        let country = text("country").unwrap_or_default().to_uppercase();
        if !store.ships_to(&country) {
            return Err(Error::field("address", "the store does not ship to this country"));
        }
        let mut shape = Doc::new();
        for (field, key) in [
            ("ship_address_line1", "line1"),
            ("ship_address_line2", "line2"),
            ("ship_city", "city"),
            ("ship_state", "state"),
            ("ship_postal_code", "postal_code"),
        ] {
            set(&mut shape, field, json!(text(key)));
        }
        set(&mut shape, "ship_country", country.clone());
        set(&mut shape, "contact_name", json!(s(so, "customer_name")));
        set(&mut shape, "billing_same_as_shipping", true);
        let mut v = Doc::new();
        set(&mut v, "commerce_shipping_address", json!(address_text(&shape, "ship")));
        if let Some(customer) = id(so, "customer")
            && let Some(place) = crate::checkout::ship_to_address(hc.kernel, hc.ctx, hc.tx, &shape, customer, false).await
        {
            set(&mut v, "ship_to", place.to_string());
        }
        set(&mut v, "delivery_country", country);
        set(&mut v, "delivery_region", json!(text("state")));
        // Taxes are worked out again for the new place, from the store's template as at checkout (a country's
        // rules may choose another for it); the rows the order has were for where it went before.
        set(&mut v, "tax_template", json!(store.tax_template.map(|t| t.to_string())));
        hc.kernel.set_values_tx(&sctx, hc.tx, "SalesOrder", so_id, v).await?;
        hc.kernel.set_child_rows_tx(&sctx, hc.tx, "SalesOrder", "taxes", so_id, vec![]).await?;
    }
    for c in &asked {
        let mut p = json!({"qty": c.qty.normalize().to_string()});
        if let Some(l) = c.line {
            p["line"] = json!(l);
        }
        if let Some(i) = c.item {
            p["item"] = json!(i.to_string());
        }
        hc.kernel.run_action_tx(&actor, hc.tx, "SalesOrder", so_id, "update_items", &p).await.map_err(for_person)?;
    }
    if asked.is_empty() {
        // Only the address moved. ERP prices an order again when a line changes and refuses a change that
        // changes nothing, so one line (the delivery charge, which holds no stock, else the first) goes up
        // by one and straight back: the order is priced for where it now goes and keeps what it had.
        let lines = rows(so, "items");
        let at = lines.iter().position(|l| store.shipping_item.is_some() && id(l, "item") == store.shipping_item).unwrap_or(0);
        let was = lines.get(at).map(|l| d(l, "qty")).unwrap_or(Decimal::ONE);
        for qty in [was + Decimal::ONE, was] {
            let p = json!({"line": at + 1, "qty": qty.normalize().to_string()});
            hc.kernel.run_action_tx(&actor, hc.tx, "SalesOrder", so_id, "update_items", &p).await.map_err(for_person)?;
        }
    }

    // Delivery follows what the order now holds and where it now goes.
    reprice_delivery(hc, &store, so_id, param_text(params, "shipping_rule", 36)?, new_address.is_some()).await?;

    // What the order comes to now, against what has been paid.
    let after = get(hc.kernel, hc.ctx, hc.tx, "SalesOrder", so_id).await?;
    let total = d(&after, "rounded_total");
    let difference = total - before;
    let mut summary = Map::new();
    let (mut requested, mut refunded) = (Value::Null, Value::Null);
    // What happens to the difference, as the desk is shown it before saving and told after.
    let mut settlement = json!({"kind": "none", "amount": money(Decimal::ZERO)});
    let label_of = |gateway: &str| store.method(gateway).map(|m| m.label.clone()).unwrap_or_else(|| gateway.to_string());
    let methods_of = |gateway: &str| store.method(gateway).cloned();
    let open = find(
        hc.kernel,
        &sctx,
        hc.tx,
        "PaymentRequest",
        vec![Filter::eq("sales_order", so_id.to_string()), Filter::eq("status", "Initiated"), Filter::eq("docstatus", 1)],
        None,
        20,
    )
    .await?;
    let mut balance = owed(hc.tx, hc.ctx.tenant, &after).await?;
    if !open.is_empty() {
        // Not paid yet: the request asked for the old total is replaced by one for the new.
        let gateway = open.first().and_then(|r| s(r, "gateway")).map(str::to_string);
        for r in &open {
            hc.kernel.cancel_tx(&sctx, hc.tx, "PaymentRequest", id(r, "id").unwrap_or_default(), None).await?;
        }
        if let Some(method) = gateway.as_deref().and_then(methods_of)
            && balance > Decimal::ZERO
        {
            let made = open_request(hc.kernel, &sctx, hc.tx, &store, &after, &method, None, balance).await?;
            requested = json!(s(&made, "id"));
            settlement = json!({"kind": "charge", "amount": money(balance), "gateway": method.gateway, "label": method.label, "replaces_open_request": true});
            balance = Decimal::ZERO;
        }
    } else if balance > Decimal::ZERO {
        // Paid, and now owing more: asked for through the method it was paid with (a card's part is not one).
        let paid_by: Option<String> = sqlx::query_scalar(
            "select gateway from e_payment_request where tenant_id = $1 and sales_order = $2 and status = 'Paid' and docstatus = 1 and gateway <> $3 order by paid_at desc nulls last limit 1",
        )
        .bind(hc.ctx.tenant)
        .bind(so_id)
        .bind(GIFT_CARD)
        .fetch_optional(&mut **hc.tx)
        .await?;
        let method = paid_by.as_deref().and_then(methods_of).or_else(|| store.payment_methods.first().cloned());
        if let Some(method) = method {
            let made = open_request(hc.kernel, &sctx, hc.tx, &store, &after, &method, None, balance).await?;
            requested = json!(s(&made, "id"));
            settlement = json!({"kind": "charge", "amount": money(balance), "gateway": method.gateway, "label": method.label, "replaces_open_request": false});
            balance = Decimal::ZERO;
        }
    } else if balance < Decimal::ZERO {
        // Paid, and now owing less: sent back through a payment that can take it (a gateway's, not a card's).
        let back = -balance;
        let found: Vec<(Uuid, Decimal)> = sqlx::query_as(
            "select id, amount from e_payment_request where tenant_id = $1 and sales_order = $2 and status = 'Paid' and docstatus = 1 and gateway <> $3 order by paid_at desc nulls last",
        )
        .bind(hc.ctx.tenant)
        .bind(so_id)
        .bind(GIFT_CARD)
        .fetch_all(&mut **hc.tx)
        .await?;
        let mut target = None;
        for (pr, amount) in found {
            let held: Decimal = sqlx::query_scalar(
                "select coalesce(sum(amount), 0) from e_order_refund where tenant_id = $1 and payment_request = $2 and status in ('Requested', 'Approved', 'Refund Failed', 'Refunded')",
            )
            .bind(hc.ctx.tenant)
            .bind(pr)
            .fetch_one(&mut **hc.tx)
            .await?;
            if amount - held >= back {
                target = Some(pr);
                break;
            }
        }
        if let Some(pr_id) = target {
            let pr = get(hc.kernel, hc.ctx, hc.tx, "PaymentRequest", pr_id).await?;
            let reason = reason_named(hc, "Order changed").await?;
            let refund = json!({
                "company": s(&after, "company"), "sales_order": so_id.to_string(), "order_number": s(&after, "number"), "customer": s(&after, "customer"),
                "kind": "Adjustment", "reason": reason.to_string(), "note": param_text(params, "note", 2000)?, "notify_customer": false,
                "payment_request": pr_id.to_string(), "gateway": s(&pr, "gateway"), "payment_reference": s(&pr, "gateway_reference"),
                "currency": s(&pr, "currency"), "currency_code": s(&pr, "currency_code"), "amount": money(back), "requested_by": hc.ctx.email.clone(), "booked": false,
            });
            let made = hc.kernel.insert_tx(&sctx, hc.tx, "OrderRefund", refund.as_object().cloned().unwrap_or_default()).await?;
            let rid = id(&made, "id").ok_or_else(|| Error::internal("refund without id"))?;
            if !matches!(param_text(params, "approve", 10)?.as_deref().map(str::to_ascii_lowercase).as_deref(), Some("no")) {
                hc.kernel.run_action_tx(&sctx, hc.tx, "OrderRefund", rid, "approve_refund", &json!({})).await?;
            }
            refunded = json!(rid.to_string());
            let sent = get(hc.kernel, &sctx, hc.tx, "OrderRefund", rid).await?;
            let gateway = s(&pr, "gateway").unwrap_or_default();
            settlement = json!({"kind": "refund", "amount": money(back), "gateway": gateway, "label": label_of(gateway), "status": s(&sent, "status")});
            balance = owed(hc.tx, hc.ctx.tenant, &after).await?;
        }
    }
    if balance != Decimal::ZERO {
        // Neither asked for nor sent back: left owing, either way.
        settlement = json!({"kind": "owed", "amount": money(balance), "by": if balance > Decimal::ZERO { "shopper" } else { "store" }});
    }
    let mut v = Doc::new();
    set(&mut v, "commerce_amount_owed", money(balance));
    hc.kernel.set_values_tx(&sctx, hc.tx, "SalesOrder", so_id, v).await?;

    // The shopper is told: what changed, what they owe or are owed, and the link where it can be paid.
    let notify = !matches!(param_text(params, "notify", 10)?.as_deref().map(str::to_ascii_lowercase).as_deref(), Some("no"));
    if notify {
        let number = s(&after, "number").unwrap_or_default().to_string();
        let fmt = |a: Decimal| crate::pages::money(&store.currency_code, &a.abs().normalize().to_string(), store.precision);
        let mut values = Map::new();
        values.insert("total".into(), json!(fmt(total)));
        values.insert("owed".into(), json!((balance > Decimal::ZERO).then(|| fmt(balance))));
        values.insert("refund".into(), json!((difference < Decimal::ZERO).then(|| fmt(difference))));
        values.insert("pay_now".into(), json!(requested.is_string()));
        values.insert("link".into(), json!(tell::order_link(hc.tx, hc.ctx.tenant, store.store_url.as_deref(), &number, so_id).await?));
        tell::message(hc.kernel, hc.tx, hc.ctx, "commerce.order_changed", so_id, s(&after, "commerce_contact_email"), values, vec![]).await?;
    }
    summary.insert("total".into(), money(total));
    summary.insert("difference".into(), money(difference));
    summary.insert("owed".into(), money(balance));
    summary.insert("payment_request".into(), requested);
    summary.insert("refund".into(), refunded);
    summary.insert("settlement".into(), settlement);
    summary.insert("lines".into(), lines_view(&get(hc.kernel, hc.ctx, hc.tx, "SalesOrder", so_id).await?, &store));
    let goods: Decimal = rows(&after, "items").iter().filter(|l| !is_delivery(&store, l)).map(|l| d(l, "amount")).sum();
    let delivery: Decimal = rows(&after, "items").iter().filter(|l| is_delivery(&store, l)).map(|l| d(l, "amount")).sum();
    summary.insert(
        "totals".into(),
        json!({"goods": money(goods), "delivery": money(delivery), "taxes": money(d(&after, "total_taxes")), "grand_total": money(d(&after, "grand_total")),
               "rounded_total": money(total), "was": money(before)}),
    );
    Ok(Value::Object(summary))
}

fn is_delivery(store: &Store, l: &Doc) -> bool {
    store.shipping_item.is_some() && id(l, "item") == store.shipping_item
}

/// The order's lines as priced, for the desk: each by its number on the order.
fn lines_view(order: &Doc, store: &Store) -> Value {
    Value::Array(
        rows(order, "items")
            .iter()
            .enumerate()
            .map(|(i, l)| {
                json!({"line": i + 1, "id": s(l, "id"), "item": s(l, "item"), "name": s(l, "item_name"), "qty": money(d(l, "qty")), "rate": money(d(l, "rate")),
                       "amount": money(d(l, "amount")), "delivery": is_delivery(store, l), "delivered_qty": money(d(l, "delivered_qty"))})
            })
            .collect(),
    )
}

/// What the desk's change would do, saved or not: the save's own work, in a transaction that is thrown away,
/// so the preview and the save cannot disagree. The lines as priced (promotions again, tax and delivery for
/// where it goes), the difference and what happens to it.
pub async fn preview(k: &Kernel, user: &Ctx, body: &Map<String, Value>) -> Result<Value> {
    user.require("SalesOrder", Action::Read)?;
    let so_id =
        body.get("sales_order").and_then(Value::as_str).and_then(|v| v.parse::<Uuid>().ok()).ok_or_else(|| Error::field("sales_order", "is required"))?;
    let mut tx = k.db.tenant_tx(user.tenant).await?;
    let ent = k.entity(user, "SalesOrder")?;
    let mut hc = HookCtx { kernel: k, ctx: user, tx: &mut tx, entity: &ent };
    let out = async {
        let so = get(hc.kernel, hc.ctx, hc.tx, "SalesOrder", so_id).await?;
        let mut params = Value::Object(body.clone());
        params["notify"] = json!("No");
        let mut out = web_update_items(&mut hc, &so, &params).await?;
        // What the save would make is not there to point at.
        if let Some(m) = out.as_object_mut() {
            m.insert("payment_request".into(), Value::Null);
            m.insert("refund".into(), Value::Null);
            m.insert("preview".into(), json!(true));
        }
        Ok(out)
    }
    .await;
    tx.rollback().await?;
    out
}

/// Delivery priced again after an order's lines or address changed: by the rule it was priced by (or the one
/// that now applies), for the goods it now holds (their value after discounts and their weight) and the place
/// it now goes. Left alone when the order was priced by no rule, when no rule is enabled any more, or when
/// the place's postal code is not known and the rule goes by postal codes.
async fn reprice_delivery(hc: &mut HookCtx<'_>, store: &Store, so_id: Uuid, chosen: Option<String>, moved: bool) -> Result<()> {
    let sctx = sys(hc.ctx);
    let (Some(item), order) = (store.shipping_item, get(hc.kernel, &sctx, hc.tx, "SalesOrder", so_id).await?) else { return Ok(()) };
    let had = id(&order, "commerce_shipping_rule");
    let rules = enabled_rules(hc.kernel, &sctx, hc.tx).await?;
    if had.is_none() || rules.is_empty() {
        return Ok(());
    }
    let lines = rows(&order, "items");
    let goods: Vec<&Doc> = lines.iter().filter(|l| id(l, "item") != Some(item)).collect();
    let subtotal: Decimal = goods.iter().map(|l| d(l, "amount")).sum();
    let items: Vec<Uuid> = goods.iter().filter_map(|l| id(l, "item")).collect();
    let weights: std::collections::HashMap<Uuid, Decimal> =
        sqlx::query_as::<_, (Uuid, Decimal)>("select item, coalesce(weight_kg, 0) from e_website_item where tenant_id = $1 and item = any($2)")
            .bind(hc.ctx.tenant)
            .bind(&items)
            .fetch_all(&mut **hc.tx)
            .await?
            .into_iter()
            .collect();
    let weight: Decimal = goods.iter().map(|l| id(l, "item").and_then(|i| weights.get(&i)).copied().unwrap_or_default() * d(l, "qty")).sum();
    let postcode: Option<String> = match id(&order, "ship_to") {
        Some(a) if hc.ctx.schema.entity("Address").is_some() => sqlx::query_scalar("select postal_code from e_address where tenant_id = $1 and id = $2")
            .bind(hc.ctx.tenant)
            .bind(a)
            .fetch_optional(&mut **hc.tx)
            .await?
            .flatten(),
        _ => None,
    };
    let (country, region) = (s(&order, "delivery_country").map(str::to_string), s(&order, "delivery_region").map(str::to_string));
    let place = Place { country: country.as_deref(), region: region.as_deref(), postcode: postcode.as_deref() };
    if postcode.is_none() && rules.iter().any(|r| !r.postal_ranges.is_empty()) {
        return Ok(());
    }
    let applicable: Vec<(&Rule, Decimal)> = rules.iter().filter_map(|r| r.charge(&place, subtotal, weight).map(|c| (r, c))).collect();
    let wanted = chosen.and_then(|c| c.parse::<Uuid>().ok()).or(had);
    let pick = wanted.and_then(|w| applicable.iter().find(|(r, _)| r.id == w)).or(if applicable.len() == 1 { applicable.first() } else { None });
    let Some((rule, charge)) = pick else {
        return Err(if applicable.is_empty() {
            Error::field(if moved { "address" } else { "changes" }, "none of the store's delivery methods reaches this address with what the order would hold")
        } else {
            Error::field("shipping_rule", "more than one delivery method applies now: choose one")
        });
    };
    let at = lines.iter().position(|l| id(l, "item") == Some(item));
    let now = at.map(|i| d(&lines[i], "price_list_rate")).unwrap_or_default();
    if Some(rule.id) == had && *charge == now && (at.is_some() || charge.is_zero()) {
        return Ok(());
    }
    let mut v = Doc::new();
    set(&mut v, "commerce_shipping_rule", rule.id.to_string());
    hc.kernel.set_values_tx(&sctx, hc.tx, "SalesOrder", so_id, v).await?;
    let mut rows_now = lines.clone();
    let line = match at {
        Some(i) => {
            set(&mut rows_now[i], "price_list_rate", money(*charge));
            i
        }
        None => {
            // Free before, charged now: the line the cart would have made.
            let mut l = Doc::new();
            set(&mut l, "item", item.to_string());
            set(&mut l, "qty", "1");
            let name: Option<String> = sqlx::query_scalar("select item_name from e_item where tenant_id = $1 and id = $2")
                .bind(hc.ctx.tenant)
                .bind(item)
                .fetch_optional(&mut **hc.tx)
                .await?;
            set(&mut l, "item_name", json!(name));
            set(&mut l, "price_list_rate", money(*charge));
            set(&mut l, "discount_percent", "0");
            if let Some(w) = id(&order, "set_warehouse") {
                set(&mut l, "warehouse", w.to_string());
            }
            rows_now.push(l);
            rows_now.len() - 1
        }
    };
    // Only what the lines' own fields hold (a read adds the row's number and the like).
    let child =
        hc.entity.children.get("items").and_then(|c| hc.ctx.schema.entity(&c.entity)).cloned().ok_or_else(|| Error::internal("an order without lines"))?;
    let rows_now: Vec<Doc> = rows_now.into_iter().map(|r| r.into_iter().filter(|(k, _)| k == "id" || child.field(k).is_some()).collect()).collect();
    hc.kernel.set_child_rows_tx(&sctx, hc.tx, "SalesOrder", "items", so_id, rows_now).await?;
    // ERP prices the order again when a line changes: the delivery line goes up by one and straight back.
    let actor = elevated(hc.ctx);
    for qty in [2, 1] {
        let p = json!({"line": line + 1, "qty": qty.to_string()});
        hc.kernel.run_action_tx(&actor, hc.tx, "SalesOrder", so_id, "update_items", &p).await.map_err(for_person)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_desk_names_lines_to_change_and_items_to_add() {
        let i = Uuid::now_v7();
        assert_eq!(changes(&json!({})).unwrap(), vec![]);
        let list = json!({"changes": format!(r#"[{{"line": 2, "qty": "0"}}, {{"item": "{i}", "qty": 3}}]"#)});
        assert_eq!(
            changes(&list).unwrap(),
            vec![Change { line: Some(2), item: None, qty: Decimal::ZERO }, Change { line: None, item: Some(i), qty: Decimal::from(3) }]
        );
        for bad in [
            json!([{"qty": 1}]),
            json!([{"line": 1, "item": i.to_string(), "qty": 1}]),
            json!([{"line": 1}]),
            json!([{"line": 1, "qty": "-1"}]),
            json!([{"item": i.to_string(), "qty": 0}]),
        ] {
            assert!(changes(&json!({"changes": bad})).is_err());
        }
        assert!(address(&json!({"address": {"line1": "1 Road", "city": "Pune"}})).is_err(), "a country is needed");
        assert!(address(&json!({"address": r#"{"line1": "1 Road", "city": "Pune", "country": "IN"}"#})).unwrap().is_some());
    }
}
