//! Subscriptions sold in the store (the `commerce_subscriptions` package, with Finance's
//! subscriptions). A website item with a plan starts a subscription when its web order is placed:
//! the order pays the first period, so Finance bills from the second. Each renewal invoice Finance
//! makes gets a payment request through the order's payment method, and the customer sees, pays and
//! cancels their own subscriptions in the portal. Cancelling the web order cancels what it started.

use crate::orders::{linked_customers, request_view};
use crate::payment;
use crate::store::Store;
use async_trait::async_trait;
use chrono::{Duration, Months, NaiveDate};
use ferrum_domain::util::{b, d, elevated, find, get, id, rows, s, sys};
use ferrum_kernel::value::parse_date;
use ferrum_kernel::{Ctx, Doc, DocHook, Error, Filter, HookCtx, Kernel, Result, Tx};
use rust_decimal::Decimal;
use serde_json::{Value, json};
use uuid::Uuid;

pub(crate) const PACKAGE: &str = "commerce_subscriptions";

/// Statuses in which a subscription's customer has what they subscribed to.
const ENTITLED: &[&str] = &["Trialling", "Active"];

/// The first day after the period of `interval` × `count` that starts on `on`.
fn next_period(on: NaiveDate, interval: &str, count: u32) -> NaiveDate {
    match interval {
        "Day" => on + Duration::days(count.into()),
        "Week" => on + Duration::weeks(count.into()),
        "Year" => on.checked_add_months(Months::new(12 * count)).unwrap_or(on),
        _ => on.checked_add_months(Months::new(count)).unwrap_or(on),
    }
}

fn day(doc: &Doc, k: &str) -> Option<NaiveDate> {
    s(doc, k).and_then(|v| parse_date(&v[..v.len().min(10)]).ok())
}

fn obj(v: Value) -> Doc {
    v.as_object().cloned().unwrap_or_default()
}

/// On web orders: their subscription lines start subscriptions; cancelling the order cancels them.
pub struct OrderHook;

#[async_trait]
impl DocHook for OrderHook {
    async fn on_submit(&self, hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        if !b(doc, "commerce_web_order") {
            return Ok(());
        }
        let (Some(so), Some(customer), Some(company)) = (id(doc, "id"), id(doc, "customer"), id(doc, "company")) else { return Ok(()) };
        let ordered = day(doc, "transaction_date").unwrap_or_else(|| hc.ctx.today());
        for line in rows(doc, "items") {
            let Some(item) = id(&line, "item") else { continue };
            let plan: Option<Uuid> = sqlx::query_scalar(
                "select csub_plan from e_website_item where tenant_id = $1 and item = $2 and csub_plan is not null order by created_at limit 1",
            )
            .bind(hc.ctx.tenant)
            .bind(item)
            .fetch_optional(&mut **hc.tx)
            .await?;
            let Some(plan) = plan else { continue };
            let p = get(hc.kernel, hc.ctx, hc.tx, "SubscriptionPlan", plan).await?;
            let count = p.get("interval_count").and_then(Value::as_u64).unwrap_or(1).clamp(1, 1000) as u32;
            let start = next_period(ordered, s(&p, "billing_interval").unwrap_or("Month"), count);
            let sub = json!({
                "company": company.to_string(), "customer": customer.to_string(), "start_date": start.to_string(),
                "plans": [{"plan": plan.to_string(), "qty": line.get("qty")}], "csub_sales_order": so.to_string(), "csub_email": doc.get("commerce_contact_email"),
            });
            hc.kernel.insert_tx(&elevated(hc.ctx), hc.tx, "Subscription", obj(sub)).await?;
        }
        Ok(())
    }

    async fn on_cancel(&self, hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        let Some(so) = id(doc, "id") else { return Ok(()) };
        let started: Vec<Uuid> = sqlx::query_scalar(
            "select id from e_subscription where tenant_id = $1 and csub_sales_order = $2 and cancellation_date is null and status <> 'Completed'",
        )
        .bind(hc.ctx.tenant)
        .bind(so)
        .fetch_all(&mut **hc.tx)
        .await?;
        for sub in started {
            hc.kernel.run_action_tx(&elevated(hc.ctx), hc.tx, "Subscription", sub, "cancel_subscription", &json!({})).await?;
        }
        Ok(())
    }
}

/// The payment method a subscription's renewals are asked through, and where the ask goes: its
/// own, else its web order's.
async fn renewal_terms(k: &Kernel, ctx: &Ctx, tx: &mut Tx, sub: &Doc) -> Result<(Option<String>, Option<String>)> {
    let order = match id(sub, "csub_sales_order") {
        Some(so) => Some(get(k, ctx, tx, "SalesOrder", so).await?),
        None => None,
    };
    let method = s(sub, "csub_payment_method").or_else(|| order.as_ref().and_then(|o| s(o, "commerce_payment_method"))).map(str::to_string);
    let email = s(sub, "csub_email").or_else(|| order.as_ref().and_then(|o| s(o, "commerce_contact_email"))).map(str::to_string);
    Ok((method, email))
}

/// On sales invoices: a store subscription's renewal invoice is asked for through the store.
pub struct RenewalHook;

#[async_trait]
impl DocHook for RenewalHook {
    async fn on_submit(&self, hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        let Some(sid) = id(doc, "fin_subscription") else { return Ok(()) };
        let sctx = sys(hc.ctx);
        let sub = get(hc.kernel, &sctx, hc.tx, "Subscription", sid).await?;
        if id(&sub, "csub_sales_order").is_none() {
            return Ok(());
        }
        let (Some(gateway), email) = renewal_terms(hc.kernel, &sctx, hc.tx, &sub).await? else { return Ok(()) };
        let store = Store::load(hc.kernel, &sctx, hc.tx).await?;
        // A method the store no longer offers leaves the invoice to be paid from the portal.
        let Some(method) = store.method(&gateway).cloned() else { return Ok(()) };
        payment::invoice_request(hc.kernel, &sctx, hc.tx, &store, doc, &method, email, None).await?;
        Ok(())
    }
}

/// Whether `customer` holds a subscription to `plan` that gives them what it sells: trialling or
/// active (not past due, cancelled or ended).
pub async fn entitled(tx: &mut Tx, tenant: Uuid, customer: Uuid, plan: Uuid) -> Result<bool> {
    Ok(sqlx::query_scalar(
        "select exists (select 1 from e_subscription s join e_subscription__plans p on p.tenant_id = s.tenant_id and p.parent_id = s.id \
         where s.tenant_id = $1 and s.customer = $2 and p.plan = $3 and s.status = any($4))",
    )
    .bind(tenant)
    .bind(customer)
    .bind(plan)
    .bind(ENTITLED.iter().map(|x| x.to_string()).collect::<Vec<_>>())
    .fetch_one(&mut **tx)
    .await?)
}

/// The signed-in user's store subscriptions: each with its plans, its status and whether it gives
/// what it sells, its period, and its renewal invoices still to pay with the request that pays each.
pub async fn own(k: &Kernel, ctx: &Ctx, tx: &mut Tx, store: &Store, user: &Ctx) -> Result<Value> {
    let customers = linked_customers(k, ctx, tx, &user.email).await?;
    let subs: Vec<Uuid> = sqlx::query_scalar(
        "select id from e_subscription where tenant_id = $1 and customer = any($2) and csub_sales_order is not null order by created_at desc limit 100",
    )
    .bind(ctx.tenant)
    .bind(&customers)
    .fetch_all(&mut **tx)
    .await?;
    let mut out = Vec::new();
    for sid in subs {
        let sub = get(k, ctx, tx, "Subscription", sid).await?;
        let mut plans = Vec::new();
        for line in rows(&sub, "plans") {
            let Some(plan) = id(&line, "plan") else { continue };
            let p = get(k, ctx, tx, "SubscriptionPlan", plan).await?;
            plans.push(json!({"plan": s(&p, "plan_name"), "qty": d(&line, "qty").normalize().to_string(), "price": d(&p, "price").normalize().to_string(),
                              "every": format!("{} {}", p.get("interval_count").and_then(Value::as_u64).unwrap_or(1), s(&p, "billing_interval").unwrap_or("Month"))}));
        }
        let mut to_pay = Vec::new();
        for inv in unpaid(k, ctx, tx, sid).await? {
            let (si, number, outstanding, due) = (id(&inv, "id").unwrap_or_default(), s(&inv, "number"), d(&inv, "outstanding_amount"), day(&inv, "due_date"));
            let request: Option<Uuid> = sqlx::query_scalar(
                "select id from e_payment_request where tenant_id = $1 and sales_invoice = $2 and docstatus = 1 and status = 'Initiated' order by created_at desc limit 1",
            )
            .bind(ctx.tenant)
            .bind(si)
            .fetch_optional(&mut **tx)
            .await?;
            let request = match request {
                Some(pr) => Some(request_view(&get(k, ctx, tx, "PaymentRequest", pr).await?, store)),
                None => None,
            };
            to_pay.push(json!({"invoice": number, "outstanding": outstanding.normalize().to_string(), "due_date": due, "payment_request": request}));
        }
        let status = s(&sub, "status").unwrap_or_default();
        out.push(json!({
            "number": s(&sub, "number"), "status": status, "entitled": ENTITLED.contains(&status), "plans": plans,
            "start_date": s(&sub, "start_date"), "current_period_start": s(&sub, "current_period_start"), "current_period_end": s(&sub, "current_period_end"),
            "next_invoice_date": s(&sub, "next_invoice_date"), "cancel_at_period_end": b(&sub, "cancel_at_period_end"), "cancellation_date": s(&sub, "cancellation_date"),
            "to_pay": to_pay,
        }));
    }
    Ok(json!({"subscriptions": out}))
}

/// A subscription's submitted invoices with something left to pay, oldest first.
async fn unpaid(k: &Kernel, ctx: &Ctx, tx: &mut Tx, sub: Uuid) -> Result<Vec<Doc>> {
    let all = find(k, ctx, tx, "SalesInvoice", vec![Filter::eq("fin_subscription", sub.to_string())], Some("posting_date asc"), 500).await?;
    Ok(all.into_iter().filter(|i| i.get("docstatus").and_then(Value::as_i64) == Some(1) && d(i, "outstanding_amount") > Decimal::ZERO).collect())
}

/// One of the signed-in user's store subscriptions, by number.
async fn own_one(k: &Kernel, ctx: &Ctx, tx: &mut Tx, user: &Ctx, number: &str) -> Result<Doc> {
    let customers = linked_customers(k, ctx, tx, &user.email).await?;
    let sid: Option<Uuid> =
        sqlx::query_scalar("select id from e_subscription where tenant_id = $1 and number = $2 and customer = any($3) and csub_sales_order is not null")
            .bind(ctx.tenant)
            .bind(number)
            .bind(&customers)
            .fetch_optional(&mut **tx)
            .await?;
    let sid = sid.ok_or_else(|| Error::NotFound("subscription".into()))?;
    get(k, ctx, tx, "Subscription", sid).await
}

/// The customer cancels: it runs to the end of the period paid for.
pub async fn cancel(k: &Kernel, ctx: &Ctx, tx: &mut Tx, user: &Ctx, number: &str) -> Result<Value> {
    let sub = own_one(k, ctx, tx, user, number).await?;
    let sid = id(&sub, "id").ok_or_else(|| Error::internal("subscription without id"))?;
    k.run_action_tx(&elevated(ctx), tx, "Subscription", sid, "cancel_subscription", &json!({"at_period_end": true})).await?;
    let sub = get(k, ctx, tx, "Subscription", sid).await?;
    Ok(json!({"number": s(&sub, "number"), "status": s(&sub, "status"), "cancellation_date": s(&sub, "cancellation_date")}))
}

/// The customer pays a renewal invoice through one of the store's methods: the open request for
/// that method, else a new one (an open request for another method stops being payable).
pub async fn pay(k: &Kernel, ctx: &Ctx, tx: &mut Tx, store: &Store, user: &Ctx, number: &str, invoice: &str, gateway: &str) -> Result<Value> {
    let sub = own_one(k, ctx, tx, user, number).await?;
    let method = store.method(gateway).cloned().ok_or_else(|| Error::field("payment_method", "choose one of the store's payment methods"))?;
    let sid = id(&sub, "id").ok_or_else(|| Error::internal("subscription without id"))?;
    let si = unpaid(k, ctx, tx, sid)
        .await?
        .into_iter()
        .find(|i| s(i, "number") == Some(invoice))
        .and_then(|i| id(&i, "id"))
        .ok_or_else(|| Error::field("invoice", "is not an unpaid invoice of this subscription"))?;
    let open: Vec<(Uuid, String)> = sqlx::query_as(
        "select id, gateway from e_payment_request where tenant_id = $1 and sales_invoice = $2 and docstatus = 1 and status = 'Initiated' \
         and (expires_at is null or expires_at > now())",
    )
    .bind(ctx.tenant)
    .bind(si)
    .fetch_all(&mut **tx)
    .await?;
    for (pr, g) in &open {
        if *g == gateway {
            return Ok(json!({"payment_request": request_view(&get(k, ctx, tx, "PaymentRequest", *pr).await?, store)}));
        }
        k.cancel_tx(ctx, tx, "PaymentRequest", *pr, None).await?;
    }
    let inv = get(k, ctx, tx, "SalesInvoice", si).await?;
    let (_, email) = renewal_terms(k, ctx, tx, &sub).await?;
    let pr = payment::invoice_request(k, ctx, tx, store, &inv, &method, email.or_else(|| Some(user.email.clone())), None)
        .await?
        .ok_or_else(|| Error::invalid("nothing is due on this invoice"))?;
    Ok(json!({"payment_request": request_view(&pr, store)}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_period_is_the_orders() {
        let d = |y, m, dd| NaiveDate::from_ymd_opt(y, m, dd).unwrap();
        assert_eq!(next_period(d(2026, 1, 31), "Month", 1), d(2026, 2, 28), "a month end stays in its month");
        assert_eq!(next_period(d(2026, 3, 15), "Month", 3), d(2026, 6, 15));
        assert_eq!(next_period(d(2024, 2, 29), "Year", 1), d(2025, 2, 28));
        assert_eq!(next_period(d(2026, 3, 15), "Week", 2), d(2026, 3, 29));
        assert_eq!(next_period(d(2026, 3, 15), "Day", 10), d(2026, 3, 25));
    }
}
