//! Shipping and invoicing a web order from the store's desk.
//!
//! The store's people hold the Website Manager role, not the Sales roles that create delivery notes
//! and invoices. These two actions do exactly those things, for web orders only, so a Website Manager
//! can run the store without being let into the rest of the books. Each runs in one transaction: a
//! shortage of stock, or a refusal by the books, leaves no half-made delivery behind.

use crate::payment::param_text;
use crate::store::Store;
use crate::{orders, token};
use async_trait::async_trait;
use ferrum_domain::util::{b, d, elevated, id, money, rows, s, set};
use ferrum_kernel::messages::{self, Message, Recipient};
use ferrum_kernel::{Ctx, Doc, DocHook, Error, HookCtx, Result, outbox};
use rust_decimal::Decimal;
use serde_json::{Value, json};
use uuid::Uuid;

/// Roles that run the store's orders.
const ROLES: [&str; 3] = ["Website Manager", "Sales Manager", "Sales User"];

/// Whether the caller runs the store's orders (administrators do).
fn may_run_orders(ctx: &Ctx) -> bool {
    ctx.is_system || ROLES.iter().any(|r| ctx.has_role(r))
}

/// "Yes" or "No" from a form, true or false from the desk; left out, it is yes.
fn yes(params: &Value, key: &str) -> bool {
    match params.get(key) {
        Some(Value::Bool(v)) => *v,
        Some(Value::String(v)) => !matches!(v.trim().to_ascii_lowercase().as_str(), "no" | "false"),
        _ => true,
    }
}

fn made_id(made: &Value) -> Result<Uuid> {
    made.get("id").and_then(Value::as_str).and_then(|v| v.parse().ok()).ok_or_else(|| Error::internal("an action made no document"))
}

fn guard(hc: &HookCtx<'_>, so: &Doc) -> Result<()> {
    if !may_run_orders(hc.ctx) {
        return Err(Error::Forbidden("shipping and invoicing web orders is for the store's managers and sales staff".into()));
    }
    if !b(so, "commerce_web_order") {
        return Err(Error::invalid("this is not a web order"));
    }
    Ok(())
}

/// Creates and submits the invoice of an order, on the caller's behalf.
async fn invoice(hc: &mut HookCtx<'_>, actor: &Ctx, so: Uuid) -> Result<Value> {
    let made = hc.kernel.run_action_tx(actor, hc.tx, "SalesOrder", so, "make_sales_invoice", &json!({})).await?;
    let sid = made_id(&made)?;
    let posted = hc.kernel.submit_tx(actor, hc.tx, "SalesInvoice", sid, None).await?;
    Ok(json!({"id": sid, "number": s(&posted, "number")}))
}

/// One line of a shipment the desk chose: an order line, how much of it, and from where.
#[derive(Debug, PartialEq)]
pub(crate) struct Pick {
    line: String,
    qty: Option<Decimal>,
    warehouse: Option<Uuid>,
}

/// `lines` from the desk: a list (or its JSON text) of `{line, qty?, warehouse?}`; none: everything left.
pub(crate) fn picks(params: &Value) -> Result<Option<Vec<Pick>>> {
    let raw = match params.get("lines") {
        None | Some(Value::Null) => return Ok(None),
        Some(Value::String(t)) if t.trim().is_empty() => return Ok(None),
        Some(Value::String(t)) => serde_json::from_str::<Value>(t).map_err(|_| Error::field("lines", "is not a JSON list of lines"))?,
        Some(v) => v.clone(),
    };
    let Value::Array(list) = raw else { return Err(Error::field("lines", "must be a list of {line, qty, warehouse}")) };
    let mut out = Vec::new();
    for v in &list {
        let line = v.get("line").and_then(Value::as_str).ok_or_else(|| Error::field("lines", "each names its order line"))?.to_string();
        let qty = match v.get("qty") {
            None | Some(Value::Null) => None,
            Some(q) => Some(
                q.as_str()
                    .map(str::to_string)
                    .or_else(|| q.as_number().map(|n| n.to_string()))
                    .and_then(|t| t.trim().parse::<Decimal>().ok())
                    .filter(|q| *q > Decimal::ZERO)
                    .ok_or_else(|| Error::field("lines", "a quantity is a number above nothing"))?,
            ),
        };
        let warehouse = match v.get("warehouse").and_then(Value::as_str) {
            Some(w) => Some(w.parse().map_err(|_| Error::field("lines", "a warehouse is named by its id"))?),
            None => None,
        };
        out.push(Pick { line, qty, warehouse });
    }
    if out.is_empty() {
        return Err(Error::field("lines", "choose what ships"));
    }
    Ok(Some(out))
}

/// A web order's fulfilment as the shopper and the desk read it.
pub(crate) fn fulfilment(per_delivered: Decimal) -> &'static str {
    if per_delivered >= Decimal::ONE_HUNDRED {
        "Fulfilled"
    } else if per_delivered > Decimal::ZERO {
        "Partly Fulfilled"
    } else {
        "Unfulfilled"
    }
}

/// Ships a web order, all that is left or the lines and quantities the desk chose (`lines`), each line
/// from its own warehouse (the one the order reserved it in, unless the desk names another of the
/// store's): one delivery note with the carrier and tracking the desk gave, submitted. The shipment
/// that completes the order invoices it too (unless the caller says no), which applies the shopper's
/// payment; an order shipped in parts stays open to changes until then. Told to the shopper through
/// `commerce.order_shipped` and the `commerce.order_shipped` message when the note says so.
pub async fn web_ship(hc: &mut HookCtx<'_>, so: &Doc, params: &Value) -> Result<Value> {
    guard(hc, so)?;
    let so_id = id(so, "id").ok_or_else(|| Error::internal("order without id"))?;
    if d(so, "per_delivered") >= Decimal::ONE_HUNDRED {
        return Err(Error::invalid("everything on this order has been delivered"));
    }
    let carrier = param_text(params, "carrier", 140)?;
    let number = param_text(params, "tracking_number", 120)?;
    let link = param_text(params, "tracking_url", 2000)?;
    if link.as_deref().is_some_and(|l| !l.starts_with("https://")) {
        return Err(Error::field("tracking_url", "must be an https:// address"));
    }
    let chosen = picks(params)?;
    let store = Store::load(hc.kernel, hc.ctx, hc.tx).await?;
    if let Some(bad) = chosen.iter().flatten().filter_map(|p| p.warehouse).find(|w| !store.warehouses.contains(w)) {
        return Err(Error::field("lines", format!("{bad} is not one of the store's warehouses")));
    }
    let actor = elevated(hc.ctx);
    let made = hc.kernel.run_action_tx(&actor, hc.tx, "SalesOrder", so_id, "make_delivery_note", &json!({})).await?;
    let dn = made_id(&made)?;
    let mut patch = Doc::new();
    if let Some(chosen) = &chosen {
        let draft = hc.kernel.get_tx(&actor, hc.tx, "DeliveryNote", dn, false).await?;
        let lines = rows(&draft, "items");
        let mut kept = Vec::new();
        for p in chosen {
            let Some(l) = lines.iter().find(|l| s(l, "so_line") == Some(p.line.as_str())) else {
                return Err(Error::field("lines", "a line chosen is not one of this order's still to ship"));
            };
            let left = d(l, "qty");
            let qty = p.qty.unwrap_or(left);
            if qty > left {
                return Err(Error::field("lines", format!("at most {} of {} is left to ship", left.normalize(), s(l, "item_name").unwrap_or("an item"))));
            }
            let mut row = json!({"id": s(l, "id"), "qty": money(qty)});
            if let Some(w) = p.warehouse {
                row["warehouse"] = json!(w.to_string());
            }
            kept.push(row);
        }
        set(&mut patch, "items", Value::Array(kept));
    }
    set(&mut patch, "commerce_notify", yes(params, "notify"));
    if let Some(c) = carrier {
        set(&mut patch, "transporter", c);
    }
    if let Some(n) = number {
        set(&mut patch, "commerce_tracking_number", n);
    }
    if let Some(l) = link {
        set(&mut patch, "commerce_tracking_url", l);
    }
    hc.kernel.update_tx(&actor, hc.tx, "DeliveryNote", dn, patch, None).await?;
    let sent = hc.kernel.submit_tx(&actor, hc.tx, "DeliveryNote", dn, None).await?;
    let after = hc.kernel.get_tx(&actor, hc.tx, "SalesOrder", so_id, false).await?;
    // The shipment that completes the order invoices it, unless a gift card's checkout already did.
    let complete = d(&after, "per_delivered") >= Decimal::ONE_HUNDRED;
    let invoiced =
        if complete && yes(params, "invoice") && d(&after, "per_billed") < Decimal::ONE_HUNDRED { Some(invoice(hc, &actor, so_id).await?) } else { None };
    Ok(json!({"delivery_note": {"id": dn, "number": s(&sent, "number")}, "fulfilment": fulfilment(d(&after, "per_delivered")), "sales_invoice": invoiced}))
}

/// Invoices a web order that has been delivered.
pub async fn web_invoice(hc: &mut HookCtx<'_>, so: &Doc) -> Result<Value> {
    guard(hc, so)?;
    let so_id = id(so, "id").ok_or_else(|| Error::internal("order without id"))?;
    let actor = elevated(hc.ctx);
    Ok(json!({"sales_invoice": invoice(hc, &actor, so_id).await?}))
}

/// The link a shopper opens to follow an order, with no sign-in. The token rides in the address'
/// fragment, which browsers never send to a server, so it reaches no log.
pub async fn tracking_link(hc: &mut HookCtx<'_>, so: &Doc) -> Result<Value> {
    guard(hc, so)?;
    let so_id = id(so, "id").ok_or_else(|| Error::internal("order without id"))?;
    let store = Store::load(hc.kernel, hc.ctx, hc.tx).await?;
    let link = match (&store.store_url, s(so, "number")) {
        (Some(url), Some(number)) => Some(orders::tracking_url(url, number, &token::order_token(hc.tx, hc.ctx.tenant, so_id).await?)),
        _ => None,
    };
    Ok(json!({"link": link}))
}

/// Tells the world a web order left: after its delivery note is submitted, `commerce.order_shipped`
/// carries the carrier and tracking, the shopper's address and the link to follow the order, for a
/// mailer or webhook to use. `notify` says whether the person asked for the shopper to be told.
pub struct DeliveryHook;

#[async_trait]
impl DocHook for DeliveryHook {
    async fn on_submit(&self, hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        if !crate::installed(hc.ctx) || b(doc, "is_return") {
            return Ok(());
        }
        let mut ids: Vec<Uuid> = rows(doc, "items").iter().filter_map(|r| id(r, "sales_order")).collect();
        ids.sort();
        ids.dedup();
        for so_id in ids {
            let so = ferrum_domain::util::get(hc.kernel, hc.ctx, hc.tx, "SalesOrder", so_id).await?;
            if !b(&so, "commerce_web_order") {
                continue;
            }
            let number = s(&so, "number").unwrap_or_default().to_string();
            let link = match Store::load(hc.kernel, hc.ctx, hc.tx).await {
                Ok(store) => match &store.store_url {
                    Some(url) => Some(orders::tracking_url(url, &number, &token::order_token(hc.tx, hc.ctx.tenant, so_id).await?)),
                    None => None,
                },
                Err(Error::NotFound(_)) => None,
                Err(e) => return Err(e),
            };
            let event = json!({
                "sales_order": so_id, "number": number, "delivery_note": s(doc, "id"), "email": s(&so, "commerce_contact_email"),
                "carrier": s(doc, "transporter"), "tracking_number": s(doc, "commerce_tracking_number"), "tracking_url": s(doc, "commerce_tracking_url"),
                "notify": b(doc, "commerce_notify"), "order_link": link, "ship_to": s(&so, "commerce_shipping_address"),
            });
            outbox::enqueue(hc.tx, hc.ctx.tenant, "commerce.order_shipped", Some("SalesOrder"), Some(so_id), event).await?;
            let mut v = Doc::new();
            set(&mut v, "commerce_fulfilment_status", fulfilment(d(&so, "per_delivered")));
            hc.kernel.set_values_tx(&ferrum_domain::util::sys(hc.ctx), hc.tx, "SalesOrder", so_id, v).await?;
            if b(doc, "commerce_notify")
                && let Some(to) = s(&so, "commerce_contact_email")
            {
                let lines: Vec<Value> = rows(doc, "items")
                    .iter()
                    .filter(|l| id(l, "sales_order") == Some(so_id))
                    .map(|l| json!(format!("{} × {}", d(l, "qty").normalize(), s(l, "item_name").unwrap_or_default())))
                    .collect();
                let mut values = serde_json::Map::new();
                for (k, v) in [
                    ("lines", Value::Array(lines)),
                    ("carrier", json!(s(doc, "transporter"))),
                    ("tracking_number", json!(s(doc, "commerce_tracking_number"))),
                    ("tracking_url", json!(s(doc, "commerce_tracking_url"))),
                    ("link", json!(link)),
                ] {
                    values.insert(k.into(), v);
                }
                let m = Message {
                    template: "commerce.order_shipped".into(),
                    entity: Some("SalesOrder".into()),
                    record: Some(so_id),
                    to: vec![Recipient::address(to)],
                    values,
                    ..Default::default()
                };
                messages::send(hc.kernel, hc.tx, hc.ctx, &m).await?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn yes_and_no_from_a_form_or_the_desk() {
        assert!(yes(&json!({}), "notify") && yes(&json!({"notify": "Yes"}), "notify") && yes(&json!({"notify": true}), "notify"));
        assert!(!yes(&json!({"notify": "No"}), "notify") && !yes(&json!({"notify": false}), "notify") && !yes(&json!({"notify": " no "}), "notify"));
    }

    #[test]
    fn the_desk_chooses_lines_quantities_and_warehouses() {
        let w = Uuid::now_v7();
        assert_eq!(picks(&json!({})).unwrap(), None, "nothing chosen: everything left");
        let text = json!({"lines": format!(r#"[{{"line": "a", "qty": "2"}}, {{"line": "b", "warehouse": "{w}"}}]"#)});
        assert_eq!(
            picks(&text).unwrap().unwrap(),
            vec![Pick { line: "a".into(), qty: Some(Decimal::TWO), warehouse: None }, Pick { line: "b".into(), qty: None, warehouse: Some(w) }]
        );
        assert!(
            picks(&json!({"lines": [{"line": "a", "qty": "0"}]})).is_err() && picks(&json!({"lines": []})).is_err() && picks(&json!({"lines": "{"})).is_err()
        );
        assert_eq!((fulfilment(Decimal::ZERO), fulfilment(Decimal::TEN), fulfilment(Decimal::ONE_HUNDRED)), ("Unfulfilled", "Partly Fulfilled", "Fulfilled"));
    }
}
