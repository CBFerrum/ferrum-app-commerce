//! Payment requests and the money side of web orders.
//!
//! A request is created and submitted at checkout (`PaymentRequest.submitted` is what gateway
//! plugins subscribe to). The gateway writes its checkout link with `set_gateway_details` and
//! confirms with `mark_paid` — idempotent, refused for another amount or currency and once the
//! request expired — which books a Payment Entry: allocated to the order's open invoices, or kept
//! as an advance that `AdvanceHook` applies when the order's invoice is submitted.

use crate::store::{PaymentMethod, Store, is_offline, notify_managers, valid_gateway};
use async_trait::async_trait;
use ferrum_domain::ledger;
use ferrum_domain::util::{b, company, currency, d, elevated, get, id, money, round, rows, s, set, sys};
use ferrum_kernel::value::parse_datetime;
use ferrum_kernel::{Ctx, Doc, DocHook, Error, HookCtx, Kernel, Result, Tx, outbox};
use rust_decimal::Decimal;
use serde_json::{Value, json};
use std::str::FromStr;
use uuid::Uuid;

const PLUGIN_DOMAIN: &str = "@plugins.ferrum.invalid";

/// Creates and submits the payment request of an order; `None` when nothing is to be paid.
pub async fn create_request(
    k: &Kernel,
    ctx: &Ctx,
    tx: &mut Tx,
    store: &Store,
    so: &Doc,
    method: &PaymentMethod,
    return_url: Option<String>,
) -> Result<Option<Doc>> {
    let so_id = id(so, "id").ok_or_else(|| Error::internal("order without id"))?;
    let amount = d(so, "rounded_total");
    if amount <= Decimal::ZERO {
        set_order_status(k, ctx, tx, so_id, "Not Required").await?;
        return Ok(None);
    }
    let cur = id(so, "currency").ok_or_else(|| Error::internal("order without currency"))?;
    let (code, _) = currency(k, ctx, tx, cur).await?;
    let number = s(so, "number").unwrap_or_default().to_string();
    let mut pr = Doc::new();
    set(&mut pr, "company", s(so, "company").unwrap_or_default());
    set(&mut pr, "sales_order", so_id.to_string());
    set(&mut pr, "order_number", number.clone());
    set(&mut pr, "customer", json!(s(so, "customer")));
    set(&mut pr, "payer_name", json!(s(so, "customer_name")));
    set(&mut pr, "payer_email", json!(s(so, "commerce_contact_email")));
    set(&mut pr, "gateway", method.gateway.clone());
    set(&mut pr, "currency", cur.to_string());
    set(&mut pr, "currency_code", code);
    set(&mut pr, "amount", money(amount));
    set(&mut pr, "status", "Initiated");
    if method.expiry_minutes > 0 {
        let at = crate::now() + chrono::Duration::minutes(method.expiry_minutes);
        set(&mut pr, "expires_at", at.to_rfc3339_opts(chrono::SecondsFormat::Millis, true));
    }
    set(&mut pr, "return_url", json!(return_url));
    set(&mut pr, "description", format!("Order {number} at {}", store.name).chars().take(200).collect::<String>());
    let draft = k.insert_tx(ctx, tx, "PaymentRequest", pr).await?;
    let pr_id = id(&draft, "id").ok_or_else(|| Error::internal("payment request without id"))?;
    let submitted = k.submit_tx(ctx, tx, "PaymentRequest", pr_id, None).await?;
    set_order_status(k, ctx, tx, so_id, "Pending").await?;
    Ok(Some(submitted))
}

async fn set_order_status(k: &Kernel, ctx: &Ctx, tx: &mut Tx, so: Uuid, status: &str) -> Result<()> {
    let mut v = Doc::new();
    set(&mut v, "commerce_payment_status", status);
    k.set_values_tx(&sys(ctx), tx, "SalesOrder", so, v).await
}

/// The order's web payment status from its requests: paid if any request is, else the latest.
pub async fn refresh_order(k: &Kernel, ctx: &Ctx, tx: &mut Tx, so: Uuid) -> Result<()> {
    let statuses: Vec<String> = sqlx::query_scalar(
        "select status from e_payment_request where tenant_id = $1 and sales_order = $2 and docstatus > 0 order by created_at desc, id desc",
    )
    .bind(ctx.tenant)
    .bind(so)
    .fetch_all(&mut **tx)
    .await?;
    let status = if statuses.iter().any(|x| x == "Paid") {
        "Paid"
    } else {
        match statuses.first().map(String::as_str) {
            Some("Initiated") => "Pending",
            Some("Failed") => "Failed",
            Some("Expired") => "Expired",
            Some("Cancelled") => "Cancelled",
            _ => return Ok(()),
        }
    };
    set_order_status(k, ctx, tx, so, status).await
}

/// Gateway plugins act as `<name>@plugins.ferrum.invalid`; one may settle only its own requests.
fn caller_may_settle(ctx: &Ctx, gateway: &str) -> Result<()> {
    match ctx.email.strip_suffix(PLUGIN_DOMAIN) {
        Some(plugin) if plugin == gateway && !is_offline(gateway) => Ok(()),
        Some(plugin) => Err(Error::Forbidden(format!("plugin '{plugin}' cannot settle payment requests of '{gateway}'"))),
        None => Ok(()),
    }
}

fn param_text(params: &Value, key: &str, max: usize) -> Result<Option<String>> {
    match params.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(v)) => {
            let v = v.trim();
            if v.chars().count() > max || v.chars().any(char::is_control) {
                return Err(Error::field(key, format!("at most {max} printable characters")));
            }
            Ok((!v.is_empty()).then(|| v.to_string()))
        }
        Some(_) => Err(Error::field(key, "must be text")),
    }
}

fn expired(doc: &Doc) -> bool {
    s(doc, "expires_at").and_then(|t| parse_datetime(t).ok()).is_some_and(|t| t <= crate::now())
}

fn paid_result(doc: &Doc, replayed: bool) -> Value {
    json!({"status": "Paid", "payment_entry": s(doc, "payment_entry"), "replayed": replayed})
}

/// The order's open invoices (oldest first) with what is outstanding on them.
async fn open_invoices(tx: &mut Tx, tenant: Uuid, customer: Uuid, so: Uuid) -> Result<Vec<(Uuid, Decimal)>> {
    Ok(sqlx::query_as(
        "select si.id, si.outstanding_amount from e_sales_invoice si where si.tenant_id = $1 and si.docstatus = 1 and si.customer = $2 \
         and si.outstanding_amount > 0 and exists (select 1 from e_sales_invoice__items it where it.tenant_id = si.tenant_id and it.parent_id = si.id \
         and it.sales_order = $3) order by si.posting_date, si.created_at",
    )
    .bind(tenant)
    .bind(customer)
    .bind(so)
    .fetch_all(&mut **tx)
    .await?)
}

async fn mark_paid(hc: &mut HookCtx<'_>, doc: &Doc, params: &Value) -> Result<Value> {
    let gateway = s(doc, "gateway").unwrap_or_default().to_string();
    caller_may_settle(hc.ctx, &gateway)?;
    let reference = param_text(params, "gateway_reference", 200)?;
    match s(doc, "status") {
        Some("Paid") => {
            if let (Some(r), Some(existing)) = (&reference, s(doc, "gateway_reference"))
                && r != existing
            {
                return Err(Error::Conflict(format!("this request was already paid (reference {existing}); refund payment {r}")));
            }
            return Ok(paid_result(doc, true));
        }
        Some(st @ ("Cancelled" | "Expired")) => {
            return Err(Error::invalid(format!("this payment request is {} and cannot be paid; refund the payment", st.to_lowercase())));
        }
        _ => {}
    }
    if expired(doc) {
        return Err(Error::invalid("this payment request has expired and cannot be paid; refund the payment"));
    }
    let amount = params
        .get("amount")
        .and_then(|v| v.as_str().map(str::to_string).or_else(|| v.as_number().map(|n| n.to_string())))
        .and_then(|t| Decimal::from_str(t.trim()).ok())
        .ok_or_else(|| Error::field("amount", "the amount paid is required (a decimal string)"))?;
    if amount != d(doc, "amount") {
        return Err(Error::invalid(format!("paid amount {} does not match the requested {}", amount.normalize(), d(doc, "amount").normalize())));
    }
    let paid_currency = param_text(params, "currency", 3)?.ok_or_else(|| Error::field("currency", "the currency paid is required"))?;
    if !paid_currency.eq_ignore_ascii_case(s(doc, "currency_code").unwrap_or_default()) {
        return Err(Error::invalid(format!("paid currency {paid_currency} does not match the requested {}", s(doc, "currency_code").unwrap_or_default())));
    }
    let sctx = sys(hc.ctx);
    let pr_id = id(doc, "id").ok_or_else(|| Error::internal("payment request without id"))?;
    let so_id = id(doc, "sales_order").ok_or_else(|| Error::internal("payment request without order"))?;
    // Locked: a cancellation of the order and this payment cannot cross.
    let so = hc.kernel.get_tx(&sctx, hc.tx, "SalesOrder", so_id, true).await?;
    if d(&so, "docstatus") != Decimal::ONE {
        return Err(Error::invalid("the order was cancelled; refund the payment"));
    }
    let customer = id(&so, "customer").ok_or_else(|| Error::internal("order without customer"))?;
    let store = Store::load(hc.kernel, &sctx, hc.tx).await?;
    let co = company(hc.kernel, &sctx, hc.tx, id(doc, "company").unwrap_or_default()).await?;
    let bank = store
        .method(&gateway)
        .and_then(|m| m.account)
        .or(co.account("default_bank_account"))
        .ok_or_else(|| Error::invalid("set a payment account on this payment method or the company's default bank account"))?;
    let mut budget = amount;
    let mut references = Vec::new();
    for (invoice, outstanding) in open_invoices(hc.tx, hc.ctx.tenant, customer, so_id).await? {
        let take = outstanding.min(budget);
        if take <= Decimal::ZERO {
            break;
        }
        budget -= take;
        references.push(json!({"invoice_type": "Sales Invoice", "invoice": invoice.to_string(), "allocated_amount": money(take)}));
    }
    let number = s(doc, "number").unwrap_or_default();
    let mut pe = Doc::new();
    set(&mut pe, "company", co.id.to_string());
    set(&mut pe, "posting_date", crate::today().to_string());
    set(&mut pe, "payment_type", "Receive");
    set(&mut pe, "customer", customer.to_string());
    set(&mut pe, "bank_account", bank.to_string());
    set(&mut pe, "currency", s(doc, "currency").unwrap_or_default());
    set(&mut pe, "paid_amount", money(amount));
    set(&mut pe, "reference_no", reference.clone().unwrap_or_else(|| number.to_string()));
    set(&mut pe, "reference_date", crate::today().to_string());
    set(&mut pe, "references", Value::Array(references));
    set(&mut pe, "remarks", format!("Web order {} paid via {gateway} ({number})", s(&so, "number").unwrap_or_default()));
    set(&mut pe, "commerce_sales_order", so_id.to_string());
    set(&mut pe, "commerce_payment_request", pr_id.to_string());
    // Booked on behalf of the caller (gateway service user or staff): it shows in the audit trail.
    let actor = elevated(hc.ctx);
    let draft = hc.kernel.insert_tx(&actor, hc.tx, "PaymentEntry", pe).await?;
    let pe_id = id(&draft, "id").ok_or_else(|| Error::internal("payment entry without id"))?;
    let posted = hc.kernel.submit_tx(&actor, hc.tx, "PaymentEntry", pe_id, None).await?;
    let mut v = Doc::new();
    set(&mut v, "status", "Paid");
    set(&mut v, "paid_at", crate::now_str());
    set(&mut v, "payment_entry", pe_id.to_string());
    if let Some(r) = &reference {
        set(&mut v, "gateway_reference", r.clone());
    }
    hc.kernel.set_values_tx(&sctx, hc.tx, "PaymentRequest", pr_id, v).await?;
    refresh_order(hc.kernel, &sctx, hc.tx, so_id).await?;
    let order_number = s(&so, "number").unwrap_or_default().to_string();
    notify_managers(
        hc.tx,
        hc.ctx.tenant,
        &format!("Payment received for web order {order_number}"),
        Some(&format!("{} {} via {gateway}", s(doc, "currency_code").unwrap_or_default(), amount.normalize())),
        "SalesOrder",
        so_id,
    )
    .await?;
    let event = json!({"sales_order": so_id, "number": order_number, "payment_request": pr_id, "payment_entry": pe_id, "amount": money(amount),
                       "currency": s(doc, "currency_code"), "gateway": gateway, "email": s(doc, "payer_email")});
    outbox::enqueue(hc.tx, hc.ctx.tenant, "commerce.order_paid", Some("SalesOrder"), Some(so_id), event).await?;
    Ok(json!({"status": "Paid", "payment_entry": pe_id, "payment_entry_number": s(&posted, "number"), "replayed": false}))
}

async fn mark_failed(hc: &mut HookCtx<'_>, doc: &Doc, params: &Value) -> Result<Value> {
    let gateway = s(doc, "gateway").unwrap_or_default().to_string();
    caller_may_settle(hc.ctx, &gateway)?;
    match s(doc, "status") {
        Some("Paid") => return Err(Error::Conflict("this request is already paid".into())),
        Some("Initiated") => {}
        other => return Ok(json!({"status": other})),
    }
    let reason = param_text(params, "reason", 2000)?;
    let reference = param_text(params, "gateway_reference", 200)?;
    let sctx = sys(hc.ctx);
    let pr_id = id(doc, "id").ok_or_else(|| Error::internal("payment request without id"))?;
    let mut v = Doc::new();
    set(&mut v, "status", "Failed");
    set(&mut v, "failure_reason", json!(reason));
    if let Some(r) = reference {
        set(&mut v, "gateway_reference", r);
    }
    hc.kernel.set_values_tx(&sctx, hc.tx, "PaymentRequest", pr_id, v).await?;
    if let Some(so) = id(doc, "sales_order") {
        refresh_order(hc.kernel, &sctx, hc.tx, so).await?;
        let event = json!({"sales_order": so, "number": s(doc, "order_number"), "payment_request": pr_id, "reason": reason, "email": s(doc, "payer_email")});
        outbox::enqueue(hc.tx, hc.ctx.tenant, "commerce.payment_failed", Some("SalesOrder"), Some(so), event).await?;
    }
    Ok(json!({"status": "Failed"}))
}

async fn set_gateway_details(hc: &mut HookCtx<'_>, doc: &Doc, params: &Value) -> Result<Value> {
    let gateway = s(doc, "gateway").unwrap_or_default().to_string();
    caller_may_settle(hc.ctx, &gateway)?;
    if !matches!(s(doc, "status"), Some("Initiated" | "Failed")) || expired(doc) {
        return Err(Error::invalid("this payment request is no longer open"));
    }
    let url = param_text(params, "payment_url", 2000)?;
    if let Some(u) = &url
        && !u.starts_with("https://")
    {
        return Err(Error::field("payment_url", "must be an https:// address"));
    }
    let reference = param_text(params, "gateway_reference", 200)?;
    let mut v = Doc::new();
    if let Some(u) = url {
        set(&mut v, "payment_url", u);
    }
    if let Some(r) = reference {
        set(&mut v, "gateway_reference", r);
    }
    if !v.is_empty() {
        let pr_id = id(doc, "id").ok_or_else(|| Error::internal("payment request without id"))?;
        hc.kernel.set_values_tx(&sys(hc.ctx), hc.tx, "PaymentRequest", pr_id, v).await?;
    }
    Ok(json!({"status": s(doc, "status")}))
}

pub struct PaymentRequestHook;

#[async_trait]
impl DocHook for PaymentRequestHook {
    async fn validate(&self, hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        if d(doc, "amount") <= Decimal::ZERO {
            return Err(Error::field("amount", "must be greater than zero"));
        }
        if !s(doc, "gateway").is_some_and(valid_gateway) {
            return Err(Error::field("gateway", "use a gateway plugin's name, Manual or Cash on Delivery"));
        }
        let so = get(hc.kernel, hc.ctx, hc.tx, "SalesOrder", id(doc, "sales_order").ok_or_else(|| Error::field("sales_order", "is required"))?).await?;
        if d(&so, "docstatus") != Decimal::ONE {
            return Err(Error::field("sales_order", "must be a submitted order"));
        }
        if id(&so, "currency") != id(doc, "currency") || id(doc, "customer").is_some_and(|c| Some(c) != id(&so, "customer")) {
            return Err(Error::field("sales_order", "the request's currency and customer must be the order's"));
        }
        Ok(())
    }

    async fn before_cancel(&self, _hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        if s(doc, "status") == Some("Paid") {
            return Err(Error::invalid("this request is paid: cancel (refund) its payment entry first"));
        }
        Ok(())
    }

    async fn on_cancel(&self, hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        let sctx = sys(hc.ctx);
        if matches!(s(doc, "status"), Some("Initiated" | "Failed")) {
            let mut v = Doc::new();
            set(&mut v, "status", "Cancelled");
            hc.kernel.set_values_tx(&sctx, hc.tx, "PaymentRequest", id(doc, "id").unwrap_or_default(), v).await?;
        }
        if let Some(so) = id(doc, "sales_order") {
            refresh_order(hc.kernel, &sctx, hc.tx, so).await?;
        }
        Ok(())
    }

    async fn action(&self, hc: &mut HookCtx<'_>, name: &str, doc: &Doc, params: &Value) -> Result<Option<Value>> {
        Ok(Some(match name {
            "mark_paid" => mark_paid(hc, doc, params).await?,
            "mark_failed" => mark_failed(hc, doc, params).await?,
            "set_gateway_details" => set_gateway_details(hc, doc, params).await?,
            _ => return Ok(None),
        }))
    }
}

/// Web orders: refuse cancelling a paid one (refund first); cancelling releases its coupon use
/// and closes its open payment requests.
pub struct WebOrderHook;

#[async_trait]
impl DocHook for WebOrderHook {
    async fn before_cancel(&self, hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        if !crate::installed(hc.ctx) || !b(doc, "commerce_web_order") {
            return Ok(());
        }
        let paid: bool = sqlx::query_scalar(
            "select exists(select 1 from e_payment_request r join e_payment_entry p on p.tenant_id = r.tenant_id and p.id = r.payment_entry \
             where r.tenant_id = $1 and r.sales_order = $2 and r.status = 'Paid' and p.docstatus = 1)",
        )
        .bind(hc.ctx.tenant)
        .bind(id(doc, "id"))
        .fetch_one(&mut **hc.tx)
        .await?;
        if paid {
            return Err(Error::invalid("this web order is paid: cancel (refund) its payment entry before cancelling the order"));
        }
        Ok(())
    }

    async fn on_cancel(&self, hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        if !crate::installed(hc.ctx) || !b(doc, "commerce_web_order") {
            return Ok(());
        }
        let so = id(doc, "id").ok_or_else(|| Error::internal("order without id"))?;
        sqlx::query(
            "update e_payment_request set status = 'Cancelled', updated_at = now(), version = version + 1 \
             where tenant_id = $1 and sales_order = $2 and status in ('Initiated', 'Failed')",
        )
        .bind(hc.ctx.tenant)
        .bind(so)
        .execute(&mut **hc.tx)
        .await?;
        if let Some(coupon) = id(doc, "commerce_coupon") {
            crate::coupon::release(hc.tx, hc.ctx.tenant, coupon).await?;
        }
        refresh_order(hc.kernel, &sys(hc.ctx), hc.tx, so).await
    }
}

/// A cancelled payment entry no longer pays its request.
pub struct PaymentEntryHook;

#[async_trait]
impl DocHook for PaymentEntryHook {
    async fn on_cancel(&self, hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        if !crate::installed(hc.ctx) {
            return Ok(());
        }
        let (Some(pr), Some(pe)) = (id(doc, "commerce_payment_request"), id(doc, "id")) else { return Ok(()) };
        let sctx = sys(hc.ctx);
        let request = get(hc.kernel, &sctx, hc.tx, "PaymentRequest", pr).await?;
        if s(&request, "status") == Some("Paid") && id(&request, "payment_entry") == Some(pe) {
            let mut v = Doc::new();
            set(&mut v, "status", "Cancelled");
            hc.kernel.set_values_tx(&sctx, hc.tx, "PaymentRequest", pr, v).await?;
            if let Some(so) = id(&request, "sales_order") {
                refresh_order(hc.kernel, &sctx, hc.tx, so).await?;
            }
        }
        Ok(())
    }
}

/// Applies a web order's advance payments to its invoice when the invoice is submitted, as if
/// the payment had referenced it (same receivable account and booked rate; no ledger posting is
/// needed). Cancelling the invoice then requires cancelling the payment first, as for any
/// allocated payment.
pub struct AdvanceHook;

#[async_trait]
impl DocHook for AdvanceHook {
    async fn on_submit(&self, hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        if !crate::installed(hc.ctx) || b(doc, "is_return") {
            return Ok(());
        }
        let mut orders: Vec<Uuid> = rows(doc, "items").iter().filter_map(|r| id(r, "sales_order")).collect();
        orders.sort();
        orders.dedup();
        let (Some(invoice), Some(customer), Some(company_id)) = (id(doc, "id"), id(doc, "customer"), id(doc, "company")) else { return Ok(()) };
        if orders.is_empty() {
            return Ok(());
        }
        let payments: Vec<Uuid> = sqlx::query_scalar(
            "select id from e_payment_entry where tenant_id = $1 and docstatus = 1 and customer = $2 \
             and nullif(ext->>'commerce_sales_order', '')::uuid = any($3) order by posting_date, created_at",
        )
        .bind(hc.ctx.tenant)
        .bind(customer)
        .bind(&orders)
        .fetch_all(&mut **hc.tx)
        .await?;
        if payments.is_empty() {
            return Ok(());
        }
        let co = company(hc.kernel, &sys(hc.ctx), hc.tx, company_id).await?;
        let on = ferrum_domain::util::date(doc, "posting_date")?;
        let mut applied = false;
        for pe in payments {
            let Some(inv) = ledger::load_open_item(hc.tx, hc.ctx.tenant, invoice, true).await? else { break };
            if inv.outstanding <= Decimal::ZERO {
                break;
            }
            let Some(adv) = ledger::load_open_item(hc.tx, hc.ctx.tenant, pe, true).await? else { continue };
            if adv.status != "open" || adv.outstanding >= Decimal::ZERO || adv.currency != inv.currency || adv.account_id != inv.account_id {
                continue;
            }
            let amount = inv.outstanding.min(-adv.outstanding);
            let base_inv = round(amount * inv.exchange_rate, co.precision);
            let base_adv = round(amount * adv.exchange_rate, co.precision);
            if base_inv != base_adv {
                // An exchange difference needs a journal; leave the allocation to the accountant.
                continue;
            }
            ledger::allocate(hc.tx, hc.ctx, co.id, invoice, "PaymentEntry", pe, on, amount, base_inv).await?;
            ledger::allocate(hc.tx, hc.ctx, co.id, pe, "PaymentEntry", pe, on, -amount, -base_adv).await?;
            applied = true;
        }
        if applied {
            ferrum_domain::flow::sync_invoice_outstanding(hc.kernel, hc.ctx, hc.tx, "SalesInvoice", invoice).await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn ctx(email: &str) -> Ctx {
        let schema = ferrum_meta::Schema { version: 1, fy_start_month: 4, ..Default::default() };
        let mut c = Ctx::system(Uuid::nil(), Arc::new(schema));
        c.email = email.into();
        c
    }

    #[test]
    fn only_the_gateways_own_plugin_settles() {
        assert!(caller_may_settle(&ctx("stripe@plugins.ferrum.invalid"), "stripe").is_ok());
        assert!(caller_may_settle(&ctx("paypal@plugins.ferrum.invalid"), "stripe").is_err());
        assert!(caller_may_settle(&ctx("stripe@plugins.ferrum.invalid"), "Manual").is_err(), "plugins never settle offline payments");
        assert!(caller_may_settle(&ctx("accounts@shop.test"), "stripe").is_ok(), "staff may record a payment the gateway confirmed");
    }

    #[test]
    fn expiry() {
        let mut doc = Doc::new();
        assert!(!expired(&doc), "no expiry");
        set(&mut doc, "expires_at", "2000-01-01T00:00:00.000Z");
        assert!(expired(&doc));
        set(&mut doc, "expires_at", "2999-01-01T00:00:00.000Z");
        assert!(!expired(&doc));
    }
}
