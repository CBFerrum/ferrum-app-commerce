//! Checkout: one transaction turns a cart into a customer, a submitted sales order and a
//! payment request. It is idempotent per cart and client key: a repeat with the same key
//! returns the same order, another key gets a conflict. Prices, totals, the customer and every
//! status come from the server; the client only chooses (payment method, details).

use crate::cart::{self, address_text, goods};
use crate::catalog::{available, stock_items};
use crate::store::Store;
use crate::{coupon, orders, payment, token};
use ferrum_domain::util::{b, d, get, id, money, rows, s, set, sys};
use ferrum_kernel::{Ctx, Doc, Error, FieldError, Kernel, Result, Tx, outbox, with_retry};
use rust_decimal::Decimal;
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use std::str::FromStr;
use uuid::Uuid;

/// Idempotency keys: 8-200 visible ASCII characters.
pub fn valid_key(key: &str) -> bool {
    (8..=200).contains(&key.len()) && key.bytes().all(|c| c.is_ascii_graphic())
}

/// A payment return address must lie under the storefront's own URL.
pub fn return_url(store: &Store, raw: Option<&Value>) -> Result<Option<String>> {
    let Some(raw) = raw.filter(|v| !v.is_null()) else { return Ok(None) };
    let url = raw.as_str().map(str::trim).ok_or_else(|| Error::field("return_url", "must be text"))?;
    let base = store.store_url.as_deref().ok_or_else(|| Error::field("return_url", "the store has no storefront URL to return to"))?;
    let base = base.trim_end_matches('/');
    let under = url.len() <= 2000 && (url == base || url.starts_with(&format!("{base}/")) || url.starts_with(&format!("{base}?")));
    if !under || url.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err(Error::field("return_url", "must be an address on the storefront"));
    }
    Ok(Some(url.to_string()))
}

fn split_name(full: &str) -> (String, Option<String>) {
    match full.trim().split_once(' ') {
        Some((first, last)) if !last.trim().is_empty() => (first.to_string(), Some(last.trim().to_string())),
        _ => (full.trim().to_string(), None),
    }
}

async fn new_customer(k: &Kernel, ctx: &Ctx, tx: &mut Tx, store: &Store, cart: &Doc, email: &str, guest: bool) -> Result<Uuid> {
    let mut c = Doc::new();
    set(&mut c, "customer_name", s(cart, "contact_name").unwrap_or(email).to_string());
    set(&mut c, "customer_type", "Individual");
    set(&mut c, "email", email.to_lowercase());
    set(&mut c, "phone", json!(s(cart, "phone")));
    set(&mut c, "shipping_address", json!(address_text(cart, "ship")));
    set(&mut c, "billing_address", json!(address_text(cart, "bill")));
    set(&mut c, "customer_group", json!(store.default_customer_group.map(|g| g.to_string())));
    set(&mut c, "commerce_guest", guest);
    let doc = k.insert_tx(ctx, tx, "Customer", c).await?;
    id(&doc, "id").ok_or_else(|| Error::internal("customer without id"))
}

/// The customer an order belongs to. A signed-in user proved their email: their linked
/// customer, or a new one. A guest never attaches to an existing customer — that would hand
/// them its history and addresses — so every guest checkout makes a new guest customer.
async fn resolve_customer(k: &Kernel, ctx: &Ctx, tx: &mut Tx, store: &Store, cart: &Doc, user: Option<&Ctx>) -> Result<(Uuid, bool)> {
    if let Some(u) = user {
        if let Some(c) = orders::linked_customers(k, ctx, tx, &u.email).await?.first() {
            return Ok((*c, false));
        }
        return Ok((new_customer(k, ctx, tx, store, cart, &u.email, false).await?, true));
    }
    let email = s(cart, "email").ok_or_else(|| Error::field("email", "is required to check out"))?.to_string();
    Ok((new_customer(k, ctx, tx, store, cart, &email, true).await?, true))
}

/// CRM contact and addresses for a new customer when the tenant runs the CRM package. Best
/// effort inside a savepoint: the order never fails because of them.
async fn crm_records(k: &Kernel, ctx: &Ctx, tx: &mut Tx, customer: Uuid, cart: &Doc) {
    if ctx.schema.entity("Contact").is_none() && ctx.schema.entity("Address").is_none() {
        return;
    }
    if sqlx::query("savepoint commerce_crm").execute(&mut **tx).await.is_err() {
        return;
    }
    let result = crm_inserts(k, ctx, tx, customer, cart).await;
    let end = if let Err(e) = &result {
        tracing::warn!(%customer, "web checkout could not create CRM contact/addresses: {e}");
        "rollback to savepoint commerce_crm"
    } else {
        "release savepoint commerce_crm"
    };
    let _ = sqlx::query(end).execute(&mut **tx).await;
}

async fn crm_inserts(k: &Kernel, ctx: &Ctx, tx: &mut Tx, customer: Uuid, cart: &Doc) -> Result<()> {
    if ctx.schema.entity("Contact").is_some() {
        let (first, last) = split_name(s(cart, "contact_name").unwrap_or_default());
        let mut c = Doc::new();
        set(&mut c, "first_name", first);
        set(&mut c, "last_name", json!(last));
        set(&mut c, "customer", customer.to_string());
        if let Some(e) = s(cart, "email") {
            set(&mut c, "emails", json!([{"email": e, "is_primary": true}]));
        }
        if let Some(p) = s(cart, "phone") {
            set(&mut c, "phones", json!([{"phone": p, "is_primary": true}]));
        }
        k.insert_tx(ctx, tx, "Contact", c).await?;
    }
    if ctx.schema.entity("Address").is_some() {
        let same = b(cart, "billing_same_as_shipping");
        for (kind, prefix) in [("Shipping", "ship"), ("Billing", if same { "ship" } else { "bill" })] {
            let mut a = Doc::new();
            set(&mut a, "address_type", kind);
            set(&mut a, "address_line1", json!(s(cart, &format!("{prefix}_address_line1"))));
            set(&mut a, "address_line2", json!(s(cart, &format!("{prefix}_address_line2"))));
            for part in ["city", "state", "postal_code", "country"] {
                set(&mut a, part, json!(s(cart, &format!("{prefix}_{part}"))));
            }
            set(&mut a, "email", json!(s(cart, "email")));
            set(&mut a, "phone", json!(s(cart, "phone")));
            set(&mut a, "is_primary", true);
            set(&mut a, "customer", customer.to_string());
            k.insert_tx(ctx, tx, "Address", a).await?;
        }
    }
    Ok(())
}

/// Re-checks every line against the catalog as it is now: published, priced, within limits and,
/// when the store enforces stock, available — under per-item locks, so two checkouts cannot both
/// take the last unit.
async fn verify_lines(k: &Kernel, ctx: &Ctx, tx: &mut Tx, store: &Store, cart: &Doc) -> Result<()> {
    let lines = goods(cart);
    let mut errors = Vec::new();
    let mut wanted: BTreeMap<Uuid, Decimal> = BTreeMap::new();
    for (i, l) in lines.iter().enumerate() {
        let name = s(l, "item_name").unwrap_or("an item").to_string();
        let wi = match id(l, "website_item") {
            Some(w) => cart::website_item(k, ctx, tx, &w.to_string()).await,
            None => Err(Error::NotFound("item".into())),
        };
        let wi = match wi {
            Ok(wi) => wi,
            Err(Error::Validation(_) | Error::NotFound(_)) => {
                errors.push(FieldError::new("items", format!("{name} is no longer available; remove it from the cart")).in_row("items", i));
                continue;
            }
            Err(e) => return Err(e),
        };
        if d(l, "price_list_rate") <= Decimal::ZERO {
            errors.push(FieldError::new("items", format!("{name} is not for sale")).in_row("items", i));
        }
        if let Err(Error::Validation(f)) = cart::check_qty(k, ctx, tx, store, &wi, d(l, "qty")).await {
            errors.extend(f.into_iter().map(|f| FieldError::new("items", format!("{name}: {}", f.message)).in_row("items", i)));
        }
        if let Some(item) = id(l, "item") {
            *wanted.entry(item).or_default() += d(l, "qty");
        }
    }
    if store.enforce_stock {
        let items: Vec<Uuid> = wanted.keys().copied().collect();
        let stocked = stock_items(tx, ctx.tenant, &items).await?;
        // Sorted (BTreeMap) so concurrent checkouts take the locks in the same order.
        for item in wanted.keys().filter(|i| stocked.contains(i)) {
            sqlx::query("select pg_advisory_xact_lock(hashtextextended($1, 0))")
                .bind(format!("commerce:stock:{}:{item}", ctx.tenant))
                .execute(&mut **tx)
                .await?;
        }
        let ids: Vec<Uuid> = stocked.iter().copied().collect();
        let avail = available(tx, ctx.tenant, store, &ids).await?;
        for (i, l) in lines.iter().enumerate() {
            let Some(item) = id(l, "item").filter(|it| stocked.contains(it)) else { continue };
            let have = avail.get(&item).copied().unwrap_or_default().max(Decimal::ZERO);
            if wanted.get(&item).copied().unwrap_or_default() > have {
                let name = s(l, "item_name").unwrap_or("an item");
                let msg = if have.is_zero() {
                    format!("{name} is out of stock")
                } else if store.show_stock_quantity {
                    format!("only {} of {name} available", have.normalize())
                } else {
                    format!("not enough stock of {name}")
                };
                errors.push(FieldError::new("items", msg).in_row("items", i));
            }
        }
    }
    if errors.is_empty() { Ok(()) } else { Err(Error::Validation(errors)) }
}

async fn create_order(k: &Kernel, ctx: &Ctx, tx: &mut Tx, store: &Store, cart: &Doc, customer: Uuid) -> Result<Doc> {
    let items: Vec<Value> = rows(cart, "items")
        .iter()
        .map(|l| {
            // Explicit prices and discounts: the order is priced exactly like the cart was.
            let discount = if l.get("discount_percent").is_none_or(Value::is_null) { money(Decimal::ZERO) } else { money(d(l, "discount_percent")) };
            json!({"item": s(l, "item"), "qty": money(d(l, "qty")), "price_list_rate": money(d(l, "price_list_rate")), "discount_percent": discount,
                   "warehouse": store.fulfilment_warehouse.to_string()})
        })
        .collect();
    let paid_online = d(cart, "rounded_total") > Decimal::ZERO;
    let mut so = Doc::new();
    for f in ["company", "currency", "conversion_rate", "price_list", "tax_template"] {
        so.insert(f.into(), cart.get(f).cloned().unwrap_or(Value::Null));
    }
    set(&mut so, "customer", customer.to_string());
    set(&mut so, "posting_date", crate::today().to_string());
    set(&mut so, "set_warehouse", store.fulfilment_warehouse.to_string());
    set(&mut so, "items", Value::Array(items));
    set(&mut so, "commerce_web_order", true);
    set(&mut so, "commerce_payment_status", if paid_online { "Pending" } else { "Not Required" });
    set(&mut so, "commerce_contact_email", json!(s(cart, "email")));
    set(&mut so, "commerce_contact_phone", json!(s(cart, "phone")));
    set(&mut so, "commerce_shipping_address", json!(address_text(cart, "ship")));
    set(&mut so, "commerce_billing_address", json!(address_text(cart, "bill")));
    set(&mut so, "commerce_shipping_rule", json!(s(cart, "shipping_rule")));
    set(&mut so, "commerce_coupon", json!(s(cart, "coupon")));
    set(&mut so, "commerce_notes", json!(s(cart, "notes")));
    let draft = k.insert_tx(ctx, tx, "SalesOrder", so).await?;
    let so_id = id(&draft, "id").ok_or_else(|| Error::internal("order without id"))?;
    k.submit_tx(ctx, tx, "SalesOrder", so_id, None).await
}

pub struct Checkout<'a> {
    pub tenant: Uuid,
    pub user: Option<&'a Ctx>,
    pub cart_token: &'a str,
    pub key: &'a str,
    pub body: &'a Map<String, Value>,
}

pub async fn checkout(k: &Kernel, c: Checkout<'_>) -> Result<Value> {
    if !valid_key(c.key) {
        return Err(Error::field("idempotency_key", "send an Idempotency-Key of 8-200 characters"));
    }
    let expected = match c.body.get("expected_total") {
        None | Some(Value::Null) => None,
        Some(v) => Some(
            v.as_str()
                .map(str::to_string)
                .or_else(|| v.as_number().map(|n| n.to_string()))
                .and_then(|t| Decimal::from_str(t.trim()).ok())
                .ok_or_else(|| Error::field("expected_total", "must be a decimal string"))?,
        ),
    };
    let schema = k.registry.schema(c.tenant).await?;
    with_retry(|| async {
        let ctx = Ctx::system(c.tenant, schema.clone());
        let mut tx = k.db.tenant_tx(c.tenant).await?;
        let out = run(k, &ctx, &mut tx, &c, expected).await?;
        tx.commit().await?;
        Ok(out)
    })
    .await
}

async fn run(k: &Kernel, ctx: &Ctx, tx: &mut Tx, c: &Checkout<'_>, expected: Option<Decimal>) -> Result<Value> {
    let mut cart = cart::locate(k, ctx, tx, c.cart_token).await?;
    let store = Store::load(k, ctx, tx).await?;
    match s(&cart, "status") {
        Some("Ordered") if s(&cart, "checkout_key") == Some(c.key) => {
            let so = id(&cart, "sales_order").ok_or_else(|| Error::internal("ordered cart without order"))?;
            return response(k, ctx, tx, &store, so).await;
        }
        Some("Ordered") => return Err(Error::Conflict("this cart has already been checked out".into())),
        Some("Expired") => return Err(Error::Conflict("this cart has expired; start a new cart".into())),
        _ => {}
    }
    if c.user.is_none() && !store.allow_guest_checkout {
        return Err(Error::Forbidden("sign in to check out".into()));
    }
    if let Some(details) = c.body.get("details") {
        let details = details.as_object().ok_or_else(|| Error::field("details", "must be an object"))?;
        cart = cart::set_details(k, ctx, tx, &store, &cart, details).await?;
    }
    if store.terms_url.is_some() && c.body.get("accept_terms").and_then(Value::as_bool) != Some(true) {
        return Err(Error::field("accept_terms", "accept the terms to place the order"));
    }
    let gateway = c.body.get("payment_method").and_then(Value::as_str).unwrap_or_default();
    let method = store.method(gateway).cloned().ok_or_else(|| Error::field("payment_method", "choose one of the store's payment methods"))?;
    let return_url = return_url(&store, c.body.get("return_url"))?;
    let rules_exist = cart::shipping_rules_exist(tx, ctx.tenant).await?;
    let missing = cart::missing(&cart, rules_exist);
    if !missing.is_empty() {
        return Err(cart::missing_error(&missing));
    }
    if !s(&cart, "ship_country").is_some_and(|country| store.ships_to(country)) {
        return Err(Error::field("shipping_address.country", "we do not ship to this country"));
    }

    // The customer, then a final pricing pass for them (their pricing rules, today's prices).
    let (customer, new_customer) = resolve_customer(k, ctx, tx, &store, &cart, c.user).await?;
    let mut patch = Doc::new();
    set(&mut patch, "customer", customer.to_string());
    if let Some(u) = c.user {
        set(&mut patch, "user_email", u.email.to_lowercase());
    }
    let cart_id = id(&cart, "id").ok_or_else(|| Error::internal("cart without id"))?;
    let cart = k.update_tx(ctx, tx, "ShoppingCart", cart_id, patch, None).await.map_err(cart::public_errors)?;
    let missing = cart::missing(&cart, rules_exist);
    if !missing.is_empty() {
        return Err(cart::missing_error(&missing));
    }
    verify_lines(k, ctx, tx, &store, &cart).await?;
    if let Some(cid) = id(&cart, "coupon") {
        let locked = k.get_tx(ctx, tx, "CouponCode", cid, true).await?;
        let today = crate::today().to_string();
        coupon::check(tx, ctx.tenant, &locked, cart::list_subtotal(&cart, store.precision), Some(customer), s(&cart, "email"), cart_id, &today).await?;
    }
    if let Some(exp) = expected
        && exp != d(&cart, "rounded_total")
    {
        return Err(Error::Conflict(format!(
            "the cart total is now {} {} (you saw {}): review the cart and try again",
            store.currency_code,
            d(&cart, "rounded_total").normalize(),
            exp.normalize()
        )));
    }

    let so = create_order(k, ctx, tx, &store, &cart, customer).await?;
    let so_id = id(&so, "id").ok_or_else(|| Error::internal("order without id"))?;
    if d(&so, "rounded_total") != d(&cart, "rounded_total") || d(&so, "grand_total") != d(&cart, "grand_total") {
        return Err(Error::Conflict(format!(
            "the order total ({}) differs from the cart total ({}) after taxes were re-determined; review the cart",
            d(&so, "rounded_total").normalize(),
            d(&cart, "rounded_total").normalize()
        )));
    }
    let request = payment::create_request(k, ctx, tx, &store, &so, &method, return_url).await?;
    let mut done = Doc::new();
    set(&mut done, "status", "Ordered");
    set(&mut done, "sales_order", so_id.to_string());
    set(&mut done, "payment_request", json!(request.as_ref().and_then(|r| s(r, "id"))));
    set(&mut done, "checkout_key", c.key);
    set(&mut done, "ordered_at", crate::now_str());
    k.set_values_tx(ctx, tx, "ShoppingCart", cart_id, done).await?;
    if let Some(cid) = id(&cart, "coupon") {
        coupon::redeem(tx, ctx.tenant, cid).await?;
    }
    if new_customer {
        crm_records(k, ctx, tx, customer, &cart).await;
    }

    let number = s(&so, "number").unwrap_or_default().to_string();
    let total = format!("{} {}", store.currency_code, d(&so, "rounded_total").normalize());
    crate::store::notify_managers(
        tx,
        ctx.tenant,
        &format!("New web order {number}"),
        Some(&format!("{} · {total} · {}", s(&so, "customer_name").unwrap_or_default(), method.label)),
        "SalesOrder",
        so_id,
    )
    .await?;
    let order_token = token::order_token(tx, ctx.tenant, so_id).await?;
    let event = json!({
        "sales_order": so_id, "number": number, "customer": customer, "email": s(&cart, "email"), "name": s(&cart, "contact_name"),
        "currency": store.currency_code, "grand_total": money(d(&so, "rounded_total")), "payment_method": method.gateway,
        "payment_request": request.as_ref().and_then(|r| s(r, "id")), "order_token": order_token,
    });
    outbox::enqueue(tx, ctx.tenant, "commerce.order_placed", Some("SalesOrder"), Some(so_id), event).await?;
    response(k, ctx, tx, &store, so_id).await
}

/// The checkout result (also what a replay with the same key returns).
async fn response(k: &Kernel, ctx: &Ctx, tx: &mut Tx, store: &Store, so_id: Uuid) -> Result<Value> {
    let so = get(k, ctx, tx, "SalesOrder", so_id).await?;
    let view = orders::order_view(k, &sys(ctx), tx, store, &so).await?;
    let active = view["payments"].as_array().and_then(|p| p.first()).cloned().unwrap_or(Value::Null);
    let token = token::order_token(tx, ctx.tenant, so_id).await?;
    Ok(json!({"order": view, "order_token": token, "payment_request": active}))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(url: Option<&str>) -> Store {
        Store {
            name: "S".into(),
            company: Uuid::nil(),
            currency: Uuid::nil(),
            currency_code: "INR".into(),
            precision: 2,
            price_list: Uuid::nil(),
            tax_template: None,
            display_prices_with_tax: false,
            warehouses: vec![],
            fulfilment_warehouse: Uuid::nil(),
            enforce_stock: true,
            show_stock_quantity: false,
            allow_guest_checkout: true,
            default_customer_group: None,
            shipping_item: None,
            countries: vec![],
            payment_methods: vec![],
            terms_url: None,
            store_url: url.map(str::to_string),
            max_qty_per_line: Decimal::ONE_HUNDRED,
            max_cart_lines: 50,
            abandon_after_hours: 24,
            cart_expiry_days: 30,
            cancel_unpaid_orders: true,
        }
    }

    #[test]
    fn return_urls_stay_on_the_storefront() {
        let st = store(Some("https://shop.example.com/"));
        assert_eq!(return_url(&st, Some(&json!("https://shop.example.com/thanks?o=1"))).unwrap().as_deref(), Some("https://shop.example.com/thanks?o=1"));
        assert!(return_url(&st, None).unwrap().is_none());
        for bad in ["https://shop.example.com.evil.io/x", "https://evil.io/?https://shop.example.com/", "javascript:alert(1)", "https://shop.example.com/ x"] {
            assert!(return_url(&st, Some(&json!(bad))).is_err(), "{bad}");
        }
        assert!(return_url(&store(None), Some(&json!("https://shop.example.com/"))).is_err(), "no storefront URL configured");
    }

    #[test]
    fn keys_and_names() {
        assert!(valid_key("order-2026-0001") && !valid_key("short") && !valid_key("has space in it"));
        assert_eq!(split_name(" Asha  Rao "), ("Asha".to_string(), Some("Rao".to_string())));
        assert_eq!(split_name("Cher"), ("Cher".to_string(), None));
    }
}
