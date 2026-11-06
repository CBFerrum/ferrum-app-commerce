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
use ferrum_domain::util::{b, d, elevated, id, rows, s, set};
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

/// Ships everything still to deliver on a web order: one delivery note with the carrier and tracking
/// the desk gave, submitted, and (unless the caller says no) the invoice, which applies the shopper's
/// payment. Told to the shopper through `commerce.order_shipped` when the note says so.
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
    let actor = elevated(hc.ctx);
    let made = hc.kernel.run_action_tx(&actor, hc.tx, "SalesOrder", so_id, "make_delivery_note", &json!({})).await?;
    let dn = made_id(&made)?;
    let mut patch = Doc::new();
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
    let invoiced = if yes(params, "invoice") { Some(invoice(hc, &actor, so_id).await?) } else { None };
    Ok(json!({"delivery_note": {"id": dn, "number": s(&sent, "number")}, "sales_invoice": invoiced}))
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
            if b(doc, "commerce_notify") {
                let mut lines = vec![format!("Your order {number} has left us.")];
                match (s(doc, "transporter"), s(doc, "commerce_tracking_number")) {
                    (Some(c), Some(t)) => lines.push(format!("{c} has it, parcel {t}.")),
                    (Some(c), None) => lines.push(format!("{c} has it.")),
                    (None, Some(t)) => lines.push(format!("Its parcel is {t}.")),
                    (None, None) => {}
                }
                if let Some(u) = s(doc, "commerce_tracking_url") {
                    lines.push(format!("Where the carrier shows it: {u}"));
                }
                let to = s(&so, "commerce_contact_email");
                crate::tell::shopper(hc.kernel, hc.tx, hc.ctx.tenant, to, &format!("Your order {number} has shipped"), &lines, link.as_deref()).await?;
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
}
