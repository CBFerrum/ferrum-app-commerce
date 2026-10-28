//! Orders as customers see them: guest lookup (order number + email + the signed token from
//! checkout), signed-in customers' orders and invoices, reordering and paying again after a
//! failed or expired payment.
//!
//! A signed-in user is linked to the customers whose email is theirs — on the customer, or on
//! a CRM contact of it — except guest customers: their email was never verified, so whoever
//! typed it must not inherit (or expose) those orders.

use crate::cart;
use crate::store::{Store, valid_gateway};
use crate::{payment, token};
use ferrum_domain::util::{b, currency, d, find, get, id, rows, s};
use ferrum_kernel::{Ctx, Doc, Error, Filter, Kernel, ListQuery, Op, Result, Tx};
use rust_decimal::Decimal;
use serde_json::{Value, json};
use std::collections::HashMap;
use uuid::Uuid;

/// Customers a signed-in email may act for, oldest first.
pub async fn linked_customers(k: &Kernel, ctx: &Ctx, tx: &mut Tx, email: &str) -> Result<Vec<Uuid>> {
    let email = email.trim().to_lowercase();
    if email.is_empty() {
        return Ok(vec![]);
    }
    let mut out: Vec<Uuid> = Vec::new();
    let direct =
        find(k, ctx, tx, "Customer", vec![Filter::eq("email", email.clone()), Filter::eq("commerce_guest", false)], Some("created_at asc"), 50).await?;
    out.extend(direct.iter().filter(|c| !b(c, "disabled")).filter_map(|c| id(c, "id")));
    if ctx.schema.entity("Contact").is_some_and(|c| c.field("emails").is_some() && c.field("customer").is_some()) {
        let contacts = find(k, ctx, tx, "Contact", vec![Filter::eq("emails.email", email)], Some("created_at asc"), 50).await?;
        for cust in contacts.iter().filter_map(|c| id(c, "customer")) {
            if out.contains(&cust) {
                continue;
            }
            let c = get(k, ctx, tx, "Customer", cust).await?;
            if !b(&c, "commerce_guest") && !b(&c, "disabled") {
                out.push(cust);
            }
        }
    }
    Ok(out)
}

pub fn request_view(pr: &Doc, store: &Store) -> Value {
    let gateway = s(pr, "gateway").unwrap_or_default();
    let method = store.method(gateway);
    let open = s(pr, "status") == Some("Initiated");
    json!({
        "id": s(pr, "id"),
        "status": s(pr, "status"),
        "amount": d(pr, "amount").normalize().to_string(),
        "currency": s(pr, "currency_code"),
        "gateway": gateway,
        "label": method.map(|m| m.label.clone()).unwrap_or_else(|| gateway.to_string()),
        "instructions": method.and_then(|m| m.instructions.clone()),
        "payment_url": if open { s(pr, "payment_url") } else { None },
        "expires_at": s(pr, "expires_at"),
        "paid_at": s(pr, "paid_at"),
    })
}

/// An order for its customer: lines (shipping apart), totals, addresses, payments.
pub async fn order_view(k: &Kernel, ctx: &Ctx, tx: &mut Tx, store: &Store, so: &Doc) -> Result<Value> {
    let so_id = id(so, "id").ok_or_else(|| Error::internal("order without id"))?;
    let lines = rows(so, "items");
    let is_shipping = |l: &Doc| store.shipping_item.is_some() && id(l, "item") == store.shipping_item;
    let items: Vec<Uuid> = lines.iter().filter_map(|l| id(l, "item")).collect();
    let pages: HashMap<Uuid, (Option<String>, Option<String>)> =
        sqlx::query_as::<_, (Uuid, Option<String>, Option<String>)>("select item, slug, title from e_website_item where tenant_id = $1 and item = any($2)")
            .bind(ctx.tenant)
            .bind(&items)
            .fetch_all(&mut **tx)
            .await?
            .into_iter()
            .map(|(i, slug, title)| (i, (slug, title)))
            .collect();
    let m = |v: Decimal| v.normalize().to_string();
    let goods: Vec<Value> = lines
        .iter()
        .filter(|l| !is_shipping(l))
        .map(|l| {
            let (slug, title) = id(l, "item").and_then(|i| pages.get(&i)).cloned().unwrap_or_default();
            json!({"item": slug, "title": title.or_else(|| s(l, "item_name").map(str::to_string)), "qty": m(d(l, "qty")), "price": m(d(l, "rate")),
                   "amount": m(d(l, "amount")), "delivered_qty": m(d(l, "delivered_qty"))})
        })
        .collect();
    let subtotal: Decimal = lines.iter().filter(|l| !is_shipping(l)).map(|l| d(l, "amount")).sum();
    let shipping_amount: Decimal = lines.iter().filter(|l| is_shipping(l)).map(|l| d(l, "amount")).sum();
    // A rule deleted since keeps the order readable.
    let shipping_label = match id(so, "commerce_shipping_rule") {
        Some(r) => match get(k, ctx, tx, "ShippingRule", r).await {
            Ok(rule) => s(&rule, "label").map(str::to_string),
            Err(Error::NotFound(_)) => None,
            Err(e) => return Err(e),
        },
        None => None,
    };
    let code = match id(so, "currency") {
        Some(c) => currency(k, ctx, tx, c).await?.0,
        None => store.currency_code.clone(),
    };
    let requests = find(k, ctx, tx, "PaymentRequest", vec![Filter::eq("sales_order", so_id.to_string())], Some("created_at desc"), 20).await?;
    let taxes: Vec<Value> = rows(so, "taxes")
        .iter()
        .filter(|t| !d(t, "tax_amount").is_zero())
        .map(|t| json!({"description": s(t, "description"), "amount": m(if b(t, "deduct") { -d(t, "tax_amount") } else { d(t, "tax_amount") })}))
        .collect();
    Ok(json!({
        "number": s(so, "number"),
        "date": s(so, "posting_date"),
        "status": s(so, "status"),
        "delivered_percent": m(d(so, "per_delivered")),
        "payment_status": s(so, "commerce_payment_status"),
        "currency": code,
        "email": s(so, "commerce_contact_email"),
        "shipping_address": s(so, "commerce_shipping_address"),
        "billing_address": s(so, "commerce_billing_address"),
        "notes": s(so, "commerce_notes"),
        "items": goods,
        "shipping": {"method": shipping_label, "amount": m(shipping_amount)},
        "totals": {"subtotal": m(subtotal), "shipping": m(shipping_amount), "taxes": taxes, "total_taxes": m(d(so, "total_taxes")),
                   "grand_total": m(d(so, "grand_total")), "rounding_adjustment": m(d(so, "rounding_adjustment")), "rounded_total": m(d(so, "rounded_total"))},
        "payments": requests.iter().map(|r| request_view(r, store)).collect::<Vec<_>>(),
    }))
}

/// A web order by number, whole.
async fn web_order(k: &Kernel, ctx: &Ctx, tx: &mut Tx, number: &str) -> Result<Option<Doc>> {
    let number = number.trim();
    if number.is_empty() || number.len() > 64 {
        return Ok(None);
    }
    let found = find(k, ctx, tx, "SalesOrder", vec![Filter::eq("number", number), Filter::eq("commerce_web_order", true)], None, 1).await?;
    match found.first().and_then(|o| id(o, "id")) {
        Some(o) => Ok(Some(get(k, ctx, tx, "SalesOrder", o).await?)),
        None => Ok(None),
    }
}

/// Guest access: the order number, its contact email and its token must all match. Every
/// failure looks the same, so the endpoint reveals nothing about which part was wrong.
pub async fn guest_order(k: &Kernel, ctx: &Ctx, tx: &mut Tx, number: &str, email: &str, order_token: &str) -> Result<Doc> {
    let hidden = || Error::NotFound("order".into());
    let so = web_order(k, ctx, tx, number).await?.ok_or_else(hidden)?;
    let so_id = id(&so, "id").ok_or_else(hidden)?;
    let email_ok = s(&so, "commerce_contact_email").is_some_and(|e| e.eq_ignore_ascii_case(email.trim()));
    let token_ok = token::order_token_valid(tx, ctx.tenant, so_id, order_token).await?;
    if !(email_ok && token_ok) {
        return Err(hidden());
    }
    Ok(so)
}

/// A signed-in customer's own order by number.
pub async fn own_order(k: &Kernel, ctx: &Ctx, tx: &mut Tx, user: &Ctx, number: &str) -> Result<Doc> {
    let so = web_order_any(k, ctx, tx, number).await?.ok_or_else(|| Error::NotFound("order".into()))?;
    let mine = linked_customers(k, ctx, tx, &user.email).await?;
    if !id(&so, "customer").is_some_and(|c| mine.contains(&c)) {
        return Err(Error::NotFound("order".into()));
    }
    Ok(so)
}

/// Any submitted or cancelled order by number (portal customers also see orders taken by staff).
async fn web_order_any(k: &Kernel, ctx: &Ctx, tx: &mut Tx, number: &str) -> Result<Option<Doc>> {
    let number = number.trim();
    if number.is_empty() || number.len() > 64 {
        return Ok(None);
    }
    let found =
        find(k, ctx, tx, "SalesOrder", vec![Filter::eq("number", number), Filter { field: "docstatus".into(), op: Op::Gt, value: json!(0) }], None, 1).await?;
    match found.first().and_then(|o| id(o, "id")) {
        Some(o) => Ok(Some(get(k, ctx, tx, "SalesOrder", o).await?)),
        None => Ok(None),
    }
}

/// The payment request behind `id`, if the order token is the request's order's token.
pub async fn request_for_token(k: &Kernel, ctx: &Ctx, tx: &mut Tx, store: &Store, request: &str, order_token: &str) -> Result<Value> {
    let hidden = || Error::NotFound("payment request".into());
    let pr_id = Uuid::parse_str(request.trim()).map_err(|_| hidden())?;
    let pr = match get(k, ctx, tx, "PaymentRequest", pr_id).await {
        Ok(pr) => pr,
        Err(Error::NotFound(_)) => return Err(hidden()),
        Err(e) => return Err(e),
    };
    let so = id(&pr, "sales_order").ok_or_else(hidden)?;
    if !token::order_token_valid(tx, ctx.tenant, so, order_token).await? {
        return Err(hidden());
    }
    Ok(request_view(&pr, store))
}

/// A new payment request for an unpaid order (after a failed, expired or abandoned payment). An
/// open request of the same method is returned instead of a second one.
pub async fn pay_again(k: &Kernel, ctx: &Ctx, tx: &mut Tx, store: &Store, so: &Doc, gateway: &str, return_url: Option<String>) -> Result<Value> {
    if !valid_gateway(gateway) {
        return Err(Error::field("payment_method", "choose one of the store's payment methods"));
    }
    let method = store.method(gateway).cloned().ok_or_else(|| Error::field("payment_method", "choose one of the store's payment methods"))?;
    if d(so, "docstatus") != Decimal::ONE || matches!(s(so, "status"), Some("Closed" | "Cancelled")) {
        return Err(Error::invalid("this order can no longer be paid"));
    }
    let so_id = id(so, "id").ok_or_else(|| Error::internal("order without id"))?;
    let requests = find(k, ctx, tx, "PaymentRequest", vec![Filter::eq("sales_order", so_id.to_string())], Some("created_at desc"), 50).await?;
    if requests.iter().any(|r| s(r, "status") == Some("Paid")) {
        return Err(Error::invalid("this order is already paid"));
    }
    let now = crate::now();
    let open = |r: &&Doc| {
        s(r, "status") == Some("Initiated")
            && s(r, "expires_at").and_then(|t| ferrum_kernel::value::parse_datetime(t).ok()).is_none_or(|t| t > now)
            && d(r, "docstatus") == Decimal::ONE
    };
    if let Some(current) = requests.iter().find(open) {
        if s(current, "gateway") == Some(gateway) {
            return Ok(request_view(current, store));
        }
        // Switching methods: the old request stops being payable.
        k.cancel_tx(ctx, tx, "PaymentRequest", id(current, "id").unwrap_or_default(), None).await?;
    }
    let created = payment::create_request(k, ctx, tx, store, so, &method, return_url).await?;
    match created {
        Some(pr) => Ok(request_view(&pr, store)),
        None => Err(Error::invalid("nothing is due on this order")),
    }
}

async fn code_of(k: &Kernel, ctx: &Ctx, tx: &mut Tx, cache: &mut HashMap<Uuid, String>, cur: Option<Uuid>) -> Result<Option<String>> {
    let Some(c) = cur else { return Ok(None) };
    if let Some(code) = cache.get(&c) {
        return Ok(Some(code.clone()));
    }
    let code = currency(k, ctx, tx, c).await?.0;
    cache.insert(c, code.clone());
    Ok(Some(code))
}

/// A page of the signed-in customer's orders (newest first) or invoices.
pub async fn own_documents(k: &Kernel, ctx: &Ctx, user: &Ctx, entity: &str, cursor: Option<String>) -> Result<Value> {
    let mut tx = k.db.tenant_tx(ctx.tenant).await?;
    let mine = linked_customers(k, ctx, &mut tx, &user.email).await?;
    let key = if entity == "SalesOrder" { "orders" } else { "invoices" };
    if mine.is_empty() {
        tx.commit().await?;
        return Ok(json!({key: [], "next_cursor": null}));
    }
    let statuses: Vec<i64> = if entity == "SalesOrder" { vec![1, 2] } else { vec![1] };
    let q = ListQuery {
        filters: vec![
            Filter { field: "customer".into(), op: Op::In, value: json!(mine.iter().map(Uuid::to_string).collect::<Vec<_>>()) },
            Filter { field: "docstatus".into(), op: Op::In, value: json!(statuses) },
        ],
        sort: Some("created_at desc".into()),
        limit: Some(20),
        cursor,
        ..Default::default()
    };
    let page = k.list(ctx, entity, &q).await?;
    let mut cache = HashMap::new();
    let mut out = Vec::with_capacity(page.rows.len());
    for r in &page.rows {
        let code = code_of(k, ctx, &mut tx, &mut cache, id(r, "currency")).await?;
        let m = |f: &str| d(r, f).normalize().to_string();
        out.push(if entity == "SalesOrder" {
            json!({"number": s(r, "number"), "date": s(r, "posting_date"), "status": s(r, "status"), "payment_status": s(r, "commerce_payment_status"),
                   "web_order": b(r, "commerce_web_order"), "currency": code, "grand_total": m("rounded_total")})
        } else {
            json!({"number": s(r, "number"), "date": s(r, "posting_date"), "due_date": s(r, "due_date"), "status": s(r, "status"), "currency": code,
                   "grand_total": m("rounded_total"), "outstanding": m("outstanding_amount")})
        });
    }
    tx.commit().await?;
    Ok(json!({key: out, "next_cursor": page.next_cursor}))
}

/// A new cart holding an earlier order's goods at today's prices; items no longer sold are
/// listed as skipped.
pub async fn reorder(k: &Kernel, ctx: &Ctx, tx: &mut Tx, store: &Store, user: &Ctx, so: &Doc) -> Result<(String, Doc, Vec<String>)> {
    let (cart_token, mut cart) = cart::create(k, ctx, tx, Some(&user.email.to_lowercase())).await?;
    let mut skipped = Vec::new();
    for l in rows(so, "items") {
        let Some(item) = id(&l, "item") else { continue };
        if store.shipping_item == Some(item) {
            continue;
        }
        let name = s(&l, "item_name").unwrap_or("an item").to_string();
        let wid: Option<Uuid> = sqlx::query_scalar("select id from e_website_item where tenant_id = $1 and item = $2 and published")
            .bind(ctx.tenant)
            .bind(item)
            .fetch_optional(&mut **tx)
            .await?;
        let Some(wid) = wid else {
            skipped.push(name);
            continue;
        };
        match cart::add_item(k, ctx, tx, store, &cart, &wid.to_string(), d(&l, "qty")).await {
            Ok(c) => cart = c,
            Err(Error::Validation(_)) => skipped.push(name),
            Err(e) => return Err(e),
        }
    }
    Ok((cart_token, cart, skipped))
}
