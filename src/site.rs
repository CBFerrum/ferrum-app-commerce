//! The storefront API: `/api/site/{tenant}/commerce/...` for the public and
//! `/api/app/commerce/...` for signed-in customers (same routes, plus their orders and invoices).
//! Every request acts with the system context after validating its input: callers choose items,
//! quantities and details — never prices, totals, customers or statuses. What a shopper changes (a cart, a review,
//! a stock alert) is done through the gate, by the same operations an agent uses (`commerce.shopping_cart.*`,
//! `commerce.website_item.submit_review` and `.stock_alert`), each under a key: the request's `Idempotency-Key`, or
//! one made for it, so a request sent again is done once. Carts are addressed by
//! the `X-Cart-Token` header, guest orders by number + email + the `X-Order-Token` from checkout.

use crate::catalog::{self, Listing, Sort, decimal_param};
use crate::checkout::{self, Checkout};
use crate::store::Store;
use crate::{cart, edit, orders, refund, subscriptions, token};
use async_trait::async_trait;
use ferrum_app_erp::portal::customers_of;
use ferrum_domain::util::{id, s};
use ferrum_kernel::capability::{Invocation, Receipt};
use ferrum_kernel::{Ctx, Error, Kernel, Result, Site, SiteRequest, SiteResponse, Tx, with_retry};
use rust_decimal::Decimal;
use serde_json::{Map, Value, json};
use uuid::Uuid;

const MAX_BODY: usize = 64 * 1024;

pub struct Storefront;

struct Req<'a> {
    raw: &'a SiteRequest,
    /// The engine identity every storefront action runs as (after validation).
    ctx: Ctx,
    user: Option<&'a Ctx>,
}

impl Req<'_> {
    fn header(&self, name: &str) -> Option<&str> {
        self.raw.headers.get(name).and_then(Value::as_str).map(str::trim).filter(|v| !v.is_empty())
    }

    fn query(&self, name: &str) -> Option<&str> {
        self.raw.query.get(name).and_then(Value::as_str).map(str::trim).filter(|v| !v.is_empty())
    }

    fn body(&self) -> Result<Map<String, Value>> {
        if self.raw.body.iter().all(u8::is_ascii_whitespace) {
            return Ok(Map::new());
        }
        if self.raw.body.len() > MAX_BODY {
            return Err(Error::invalid("request body larger than 64 KB"));
        }
        match serde_json::from_slice::<Value>(&self.raw.body) {
            Ok(Value::Object(m)) => Ok(m),
            _ => Err(Error::invalid("the request body must be a JSON object")),
        }
    }

    fn cart_token(&self) -> Result<&str> {
        self.header("x-cart-token").ok_or_else(|| Error::invalid("send the cart token in the X-Cart-Token header"))
    }

    fn signed_in(&self) -> Result<&Ctx> {
        self.user.ok_or_else(|| Error::Unauthorized("sign in to see your orders".into()))
    }

    fn limit(&self, default: i64, max: i64) -> Result<i64> {
        match self.query("limit") {
            None => Ok(default),
            Some(v) => v.parse::<i64>().ok().filter(|n| (1..=max).contains(n)).ok_or_else(|| Error::field("limit", format!("use 1 to {max}"))),
        }
    }
}

fn text<'a>(body: &'a Map<String, Value>, key: &str) -> Result<&'a str> {
    body.get(key).and_then(Value::as_str).map(str::trim).filter(|v| !v.is_empty()).ok_or_else(|| Error::field(key, "is required"))
}

/// The signed-in caller's customer (for their prices), if any.
async fn customer_of(k: &Kernel, r: &Req<'_>, tx: &mut Tx) -> Result<Option<Uuid>> {
    match r.user {
        Some(u) => Ok(customers_of(k, &r.ctx, tx, &u.email).await?.into_iter().next()),
        None => Ok(None),
    }
}

/// A cart's operations, as their capability ids name them.
const CART: &str = "commerce.shopping_cart";

/// The key a shopper's change is done once under: the request's `Idempotency-Key`, or one made for it. Taken before
/// any retry, so a retried transaction is the same effect.
fn request_key(r: &Req<'_>) -> Result<String> {
    match r.header("idempotency-key") {
        Some(key) if checkout::valid_key(key) => Ok(key.to_string()),
        Some(_) => Err(Error::field("idempotency_key", "send an Idempotency-Key of 8-200 characters")),
        None => Ok(Uuid::now_v7().to_string()),
    }
}

/// Runs a capability through the gate, as the storefront (the system), in the request's transaction.
async fn invoke(k: &Kernel, r: &Req<'_>, tx: &mut Tx, capability: &str, input: Value, key: String) -> Result<Receipt> {
    k.invoke_tx(&r.ctx, tx, &Invocation::new(capability, input).key(key)).await
}

/// The cart behind a token: a read, by id (the operation that changes it locks it).
async fn cart_id(tx: &mut Tx, tenant: Uuid, cart_token: &str) -> Result<Uuid> {
    if !token::well_formed(cart_token) {
        return Err(Error::NotFound("cart".into()));
    }
    let found: Option<Uuid> = sqlx::query_scalar("select id from e_shopping_cart where tenant_id = $1 and token_hash = $2")
        .bind(tenant)
        .bind(token::hash(cart_token))
        .fetch_optional(&mut **tx)
        .await?;
    found.ok_or_else(|| Error::NotFound("cart".into()))
}

/// A cart is the signed-in shopper's (their customer, for their prices) or, signed out, a guest's: said through the
/// gate when the cart says otherwise.
async fn identify(k: &Kernel, r: &Req<'_>, tx: &mut Tx, cart: Uuid, key: &str) -> Result<()> {
    let doc = k.get_tx(&r.ctx, tx, "ShoppingCart", cart, false).await?;
    let want = r.user.map(|u| u.email.to_lowercase());
    let needed = match &want {
        _ if s(&doc, "user_email") != want.as_deref() => true,
        Some(email) => id(&doc, "customer").is_none() && !customers_of(k, &r.ctx, tx, email).await?.is_empty(),
        None => false,
    };
    if needed {
        invoke(k, r, tx, &format!("{CART}.identify"), json!({"id": cart, "params": {"email": want}}), format!("{cart}:{key}:identify")).await?;
    }
    Ok(())
}

/// The cart a token names, as the shopper sees it, after `act` (an operation of the cart and its params) if any.
async fn cart_op(k: &Kernel, r: &Req<'_>, act: Option<(&str, Value)>) -> Result<Value> {
    let token = r.cart_token()?;
    let key = if act.is_some() { request_key(r)? } else { String::new() };
    with_retry(|| async {
        let mut tx = k.db.tenant_tx(r.ctx.tenant).await?;
        let store = Store::load(k, &r.ctx, &mut tx).await?;
        let cart = match &act {
            None => cart::locate(k, &r.ctx, &mut tx, token).await?,
            Some((name, params)) => {
                let cid = cart_id(&mut tx, r.ctx.tenant, token).await?;
                identify(k, r, &mut tx, cid, &key).await?;
                invoke(k, r, &mut tx, &format!("{CART}.{name}"), json!({"id": cid, "params": params}), format!("{cid}:{key}")).await?;
                k.get_tx(&r.ctx, &mut tx, "ShoppingCart", cid, false).await?
            }
        };
        let view = cart::view(k, &r.ctx, &mut tx, &store, &cart).await?;
        tx.commit().await?;
        Ok(view)
    })
    .await
}

/// A new cart through the gate (its token's hash as a value; the token itself never enters an operation), the
/// shopper's when signed in. Answers its id and the token, shown this once.
async fn new_cart(k: &Kernel, r: &Req<'_>, tx: &mut Tx, key: &str) -> Result<(Uuid, String)> {
    let (cart_token, hash) = token::new_cart_token();
    let made = invoke(k, r, tx, &format!("{CART}.create"), json!({"values": {"token_hash": hash, "items": []}}), format!("cart:{key}")).await?;
    let cid = made.result.get("id").and_then(Value::as_str).and_then(|v| v.parse().ok()).ok_or_else(|| Error::internal("a cart made without an id"))?;
    identify(k, r, tx, cid, key).await?;
    Ok((cid, cart_token))
}

async fn create_cart(k: &Kernel, r: &Req<'_>) -> Result<Value> {
    let body = r.body()?;
    let lines: Vec<(String, Decimal)> = match body.get("items") {
        None | Some(Value::Null) => vec![],
        Some(Value::Array(a)) if a.len() <= 100 => a
            .iter()
            .map(|l| {
                let m = l.as_object().ok_or_else(|| Error::field("items", "each item is an object"))?;
                Ok((text(m, "item")?.to_string(), cart::qty_of(m.get("qty"), Some(Decimal::ONE))?))
            })
            .collect::<Result<_>>()?,
        Some(_) => return Err(Error::field("items", "must be a list of at most 100 items")),
    };
    let key = Uuid::now_v7().to_string();
    with_retry(|| async {
        let mut tx = k.db.tenant_tx(r.ctx.tenant).await?;
        let store = Store::load(k, &r.ctx, &mut tx).await?;
        let (cid, cart_token) = new_cart(k, r, &mut tx, &key).await?;
        for (n, (item, q)) in lines.iter().enumerate() {
            invoke(k, r, &mut tx, &format!("{CART}.add_item"), json!({"id": cid, "params": {"item": item, "qty": q.to_string()}}), format!("{cid}:{key}:{n}"))
                .await?;
        }
        let cart = k.get_tx(&r.ctx, &mut tx, "ShoppingCart", cid, false).await?;
        let view = cart::view(k, &r.ctx, &mut tx, &store, &cart).await?;
        tx.commit().await?;
        Ok(json!({"token": cart_token, "cart": view}))
    })
    .await
}

/// A new cart holding an earlier order's goods at today's prices, each line added through the gate on its own
/// savepoint: one no longer sold, or refused (out of stock, over a limit), is listed as skipped.
async fn reorder(k: &Kernel, r: &Req<'_>, user: &Ctx, number: &str) -> Result<Value> {
    let key = Uuid::now_v7().to_string();
    with_retry(|| async {
        let mut tx = k.db.tenant_tx(r.ctx.tenant).await?;
        let store = Store::load(k, &r.ctx, &mut tx).await?;
        let so = orders::own_order(k, &r.ctx, &mut tx, user, number).await?;
        let lines = orders::reorder_lines(&mut tx, r.ctx.tenant, &store, &so).await?;
        let (cid, cart_token) = new_cart(k, r, &mut tx, &key).await?;
        let mut skipped = Vec::new();
        for (n, (wid, qty, name)) in lines.into_iter().enumerate() {
            let Some(wid) = wid else {
                skipped.push(name);
                continue;
            };
            sqlx::query("savepoint reorder_line").execute(&mut *tx).await?;
            let input = json!({"id": cid, "params": {"item": wid.to_string(), "qty": qty.normalize().to_string()}});
            match invoke(k, r, &mut tx, &format!("{CART}.add_item"), input, format!("{cid}:{key}:{n}")).await {
                Ok(_) => {
                    sqlx::query("release savepoint reorder_line").execute(&mut *tx).await?;
                }
                Err(Error::Validation(_)) => {
                    sqlx::raw_sql("rollback to savepoint reorder_line; release savepoint reorder_line").execute(&mut *tx).await?;
                    skipped.push(name);
                }
                Err(e) => return Err(e),
            }
        }
        let cart = k.get_tx(&r.ctx, &mut tx, "ShoppingCart", cid, false).await?;
        let view = cart::view(k, &r.ctx, &mut tx, &store, &cart).await?;
        tx.commit().await?;
        Ok(json!({"token": cart_token, "cart": view, "skipped": skipped}))
    })
    .await
}

/// A review or a stock alert of a published item, through the gate.
async fn item_act(k: &Kernel, r: &Req<'_>, slug: &str, action: &str, params: Value) -> Result<()> {
    let key = request_key(r)?;
    with_retry(|| async {
        let mut tx = k.db.tenant_tx(r.ctx.tenant).await?;
        let wi = catalog::published_by_slug(k, &r.ctx, &mut tx, slug).await?;
        let wid = id(&wi, "id").ok_or_else(|| Error::NotFound("item".into()))?;
        let params = match (action, r.user) {
            // A signed-in shopper reviews as themselves: their email, their name unless they give one, their customer.
            ("submit_review", Some(u)) => {
                let email = u.email.to_lowercase();
                let customer = customers_of(k, &r.ctx, &mut tx, &email).await?.into_iter().next();
                let mut p = params.clone();
                let blank = match p.get("name") {
                    None | Some(Value::Null) => true,
                    Some(Value::String(v)) => v.trim().is_empty(),
                    Some(_) => false,
                };
                if blank {
                    p["name"] = json!(u.full_name);
                }
                p["email"] = json!(email);
                p["customer"] = json!(customer.map(|c| c.to_string()));
                p
            }
            _ => params.clone(),
        };
        invoke(k, r, &mut tx, &format!("commerce.website_item.{action}"), json!({"id": wid, "params": params}), format!("{wid}:{key}")).await?;
        tx.commit().await?;
        Ok(())
    })
    .await
}

async fn shipping_options(k: &Kernel, r: &Req<'_>) -> Result<Value> {
    let token = r.cart_token()?;
    let mut tx = k.db.tenant_tx(r.ctx.tenant).await?;
    let cart = cart::locate(k, &r.ctx, &mut tx, token).await?;
    let options = cart::shipping_options(k, &r.ctx, &mut tx, &cart).await?;
    tx.commit().await?;
    Ok(json!({"options": options.iter().map(|(rule, charge)| rule.view(*charge)).collect::<Vec<_>>(), "selected": s(&cart, "shipping_rule")}))
}

async fn catalog(k: &Kernel, r: &Req<'_>) -> Result<Value> {
    let q = r.query("q").map(str::to_string);
    if q.as_deref().is_some_and(|q| q.chars().count() > 100) {
        return Err(Error::field("q", "at most 100 characters"));
    }
    let f = Listing {
        category: r.query("category").map(str::to_string),
        q,
        min_price: decimal_param(r.query("min_price"), "min_price")?,
        max_price: decimal_param(r.query("max_price"), "max_price")?,
        in_stock: matches!(r.query("in_stock"), Some("1" | "true")),
        ids: None,
    };
    let sort = Sort::parse(r.query("sort"))?;
    let limit = r.limit(24, catalog::MAX_PAGE)?;
    let mut tx = k.db.tenant_tx(r.ctx.tenant).await?;
    let customer = customer_of(k, r, &mut tx).await?;
    let store = Store::load(k, &r.ctx, &mut tx).await?.for_customer(&mut tx, r.ctx.tenant, customer).await?;
    let (items, next) = catalog::cards(k, &r.ctx, &mut tx, &store, &f, sort, limit, r.query("cursor"), customer).await?;
    tx.commit().await?;
    Ok(json!({"items": items, "next_cursor": next, "currency": store.currency_code}))
}

async fn route(k: &Kernel, r: &Req<'_>) -> Result<(u16, Value)> {
    let path = r.raw.path.clone();
    let segs: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
    let ok = |v: Value| Ok((200, v));
    match (r.raw.method.as_str(), segs.as_slice()) {
        ("GET", ["catalog"]) => ok(catalog(k, r).await?),
        ("POST", ["items", slug, "reviews"]) => {
            let body = r.body()?;
            let given = |f: &str| body.get(f).cloned().unwrap_or(Value::Null);
            let params = json!({"rating": given("rating"), "title": given("title"), "review": given("review"), "name": given("name"), "email": given("email")});
            item_act(k, r, slug, "submit_review", params).await?;
            Ok((201, json!({"status": "Pending", "message": "Thank you! Your review appears once it has been approved."})))
        }
        ("POST", ["items", slug, "stock-alert"]) => {
            let body = r.body()?;
            let email = text(&body, "email")?.to_string();
            item_act(k, r, slug, "stock_alert", json!({"email": email})).await?;
            Ok((201, json!({"status": "Waiting"})))
        }
        ("POST", ["cart"]) => Ok((201, create_cart(k, r).await?)),
        ("GET", ["cart"]) => ok(cart_op(k, r, None).await?),
        ("POST", ["cart", "items"]) => {
            let body = r.body()?;
            let params = json!({"item": text(&body, "item")?, "qty": body.get("qty")});
            ok(cart_op(k, r, Some(("add_item", params))).await?)
        }
        ("PUT" | "PATCH", ["cart", "items", line]) => {
            let qty = cart::qty_of(r.body()?.get("qty"), None)?;
            ok(cart_op(k, r, Some(("set_quantity", json!({"line": line, "qty": qty.to_string()})))).await?)
        }
        ("DELETE", ["cart", "items", line]) => ok(cart_op(k, r, Some(("set_quantity", json!({"line": line, "qty": "0"})))).await?),
        ("PUT" | "PATCH", ["cart", "details"]) => {
            let details = Value::Object(r.body()?).to_string();
            ok(cart_op(k, r, Some(("set_details", json!({"details": details})))).await?)
        }
        ("PUT", ["cart", "coupon"]) => ok(cart_op(k, r, Some(("apply_code", json!({"code": text(&r.body()?, "code")?})))).await?),
        ("DELETE", ["cart", "coupon"]) => ok(cart_op(k, r, Some(("remove_code", json!({})))).await?),
        ("DELETE", ["cart", "codes", code]) => ok(cart_op(k, r, Some(("remove_code", json!({"code": code})))).await?),
        ("GET", ["cart", "shipping-options"]) => ok(shipping_options(k, r).await?),
        ("PUT", ["cart", "shipping"]) => {
            let rule = text(&r.body()?, "shipping_method")?.to_string();
            ok(cart_op(k, r, Some(("set_shipping", json!({"shipping_method": rule})))).await?)
        }
        ("POST", ["checkout"]) => {
            let body = r.body()?;
            let key = r.header("idempotency-key").or_else(|| body.get("idempotency_key").and_then(Value::as_str)).unwrap_or_default().to_string();
            let c = Checkout { tenant: r.ctx.tenant, user: r.user, cart_token: r.cart_token()?, key: &key, body: &body };
            ok(checkout::checkout(k, c).await?)
        }
        ("POST", ["orders", "pay"]) => {
            let body = r.body()?;
            ok(pay(k, r, &body, None).await?)
        }
        // The store's people, at the desk: what may come back on an order, and what a choice comes to.
        ("POST", ["desk", "refund-plan"]) => {
            let user = r.signed_in()?;
            ok(refund::plan(k, user, &r.ctx, &r.body()?).await?)
        }
        // What the desk's change of an order would do, worked out by the save's own code and thrown away.
        ("POST", ["desk", "change-preview"]) => {
            let user = r.signed_in()?;
            ok(edit::preview(k, user, &r.body()?).await?)
        }
        ("GET", ["orders"]) => {
            let user = r.signed_in()?;
            ok(orders::own_documents(k, &r.ctx, user, "SalesOrder", r.query("cursor").map(str::to_string)).await?)
        }
        ("GET", ["invoices"]) => {
            let user = r.signed_in()?;
            ok(orders::own_documents(k, &r.ctx, user, "SalesInvoice", r.query("cursor").map(str::to_string)).await?)
        }
        // Store subscriptions (Store Subscriptions installed): the customer's own, cancelled at the
        // end of the period paid for, and a renewal paid.
        ("GET", ["subscriptions"]) => {
            let user = r.signed_in()?;
            subscribed(&r.ctx)?;
            let mut tx = k.db.tenant_tx(r.ctx.tenant).await?;
            let store = Store::load(k, &r.ctx, &mut tx).await?;
            let out = subscriptions::own(k, &r.ctx, &mut tx, &store, user).await?;
            tx.commit().await?;
            ok(out)
        }
        ("POST", ["subscriptions", number, "cancel"]) => {
            let user = r.signed_in()?;
            subscribed(&r.ctx)?;
            ok(with_retry(|| async {
                let mut tx = k.db.tenant_tx(r.ctx.tenant).await?;
                let out = subscriptions::cancel(k, &r.ctx, &mut tx, user, number).await?;
                tx.commit().await?;
                Ok(out)
            })
            .await?)
        }
        ("POST", ["subscriptions", number, "pay"]) => {
            let user = r.signed_in()?;
            subscribed(&r.ctx)?;
            let body = r.body()?;
            let (invoice, gateway) = (text(&body, "invoice")?.to_string(), text(&body, "payment_method")?.to_string());
            ok(with_retry(|| async {
                let mut tx = k.db.tenant_tx(r.ctx.tenant).await?;
                let store = Store::load(k, &r.ctx, &mut tx).await?;
                let out = subscriptions::pay(k, &r.ctx, &mut tx, &store, user, number, &invoice, &gateway).await?;
                tx.commit().await?;
                Ok(out)
            })
            .await?)
        }
        ("POST", ["orders", number, "pay"]) => {
            r.signed_in()?;
            let body = r.body()?;
            ok(pay(k, r, &body, Some(number)).await?)
        }
        ("POST", ["orders", number, "reorder"]) => {
            let user = r.signed_in()?;
            Ok((201, reorder(k, r, user, number).await?))
        }
        _ => ok(read_route(k, r, &segs).await?),
    }
}

/// Read-only endpoints, in one transaction with the store loaded.
async fn read_route(k: &Kernel, r: &Req<'_>, segs: &[&str]) -> Result<Value> {
    let not_found = || Error::NotFound(format!("storefront endpoint {} {}", r.raw.method, r.raw.path));
    if r.raw.method != "GET" && r.raw.method != "POST" {
        return Err(not_found());
    }
    // The key an order token names, taken before the transaction: the vault answers on a connection of its own.
    let body = if r.raw.method == "POST" { r.body().ok() } else { None };
    let presented = match segs {
        ["payment-requests", _] => r.header("x-order-token"),
        ["orders", "lookup" | "track"] => body.as_ref().and_then(|b| b.get("token")).and_then(Value::as_str),
        _ => None,
    };
    let key = token::presented(k, r.ctx.tenant, presented).await?;
    let mut tx = k.db.tenant_tx(r.ctx.tenant).await?;
    let store = Store::load(k, &r.ctx, &mut tx).await?;
    let out = match (r.raw.method.as_str(), segs) {
        ("GET", ["store"]) => store.view(),
        ("GET", ["categories"]) => json!({"categories": catalog::category_tree(&mut tx, r.ctx.tenant).await?}),
        ("GET", ["items", slug]) => {
            let customer = customer_of(k, r, &mut tx).await?;
            let store = store.clone().for_customer(&mut tx, r.ctx.tenant, customer).await?;
            catalog::item_page(k, &r.ctx, &mut tx, &store, slug, customer).await?
        }
        ("GET", ["items", slug, "reviews"]) => {
            let limit = r.limit(10, 50)?;
            let wi = catalog::published_by_slug(k, &r.ctx, &mut tx, slug).await?;
            let (reviews, next) = catalog::reviews(&mut tx, r.ctx.tenant, id(&wi, "id").unwrap_or_default(), limit, r.query("cursor")).await?;
            json!({"reviews": reviews, "next_cursor": next})
        }
        ("GET", ["payment-requests", request]) => {
            let token = r.header("x-order-token").ok_or_else(|| Error::NotFound("payment request".into()))?;
            orders::request_for_token(k, &r.ctx, &mut tx, &store, request, token, key.as_ref()).await?
        }
        ("POST", ["orders", "lookup"]) => {
            let body = r.body()?;
            let so = orders::guest_order(k, &r.ctx, &mut tx, text(&body, "number")?, text(&body, "email")?, text(&body, "token")?, key.as_ref()).await?;
            json!({"order": orders::order_view(k, &r.ctx, &mut tx, &store, &so).await?})
        }
        // The link a shopper was sent: the number and the token alone, no email, and a thinner view.
        ("POST", ["orders", "track"]) => {
            let body = r.body()?;
            let so = orders::tracked_order(k, &r.ctx, &mut tx, text(&body, "number")?, text(&body, "token")?, key.as_ref()).await?;
            json!({"order": orders::track_view(orders::order_view(k, &r.ctx, &mut tx, &store, &so).await?)})
        }
        ("GET", ["orders", number]) => {
            let user = r.signed_in()?;
            let so = orders::own_order(k, &r.ctx, &mut tx, user, number).await?;
            json!({"order": orders::order_view(k, &r.ctx, &mut tx, &store, &so).await?})
        }
        _ => return Err(not_found()),
    };
    tx.commit().await?;
    Ok(out)
}

/// Subscriptions are answered where Store Subscriptions is installed.
fn subscribed(ctx: &Ctx) -> Result<()> {
    if ctx.schema.packages.iter().any(|p| p == subscriptions::PACKAGE) {
        Ok(())
    } else {
        Err(Error::NotFound("store subscriptions (install Store Subscriptions)".into()))
    }
}

/// Pays an unpaid order again: guests identify it like a lookup, signed-in customers by number.
async fn pay(k: &Kernel, r: &Req<'_>, body: &Map<String, Value>, own: Option<&str>) -> Result<Value> {
    let gateway = text(body, "payment_method")?.to_string();
    // The key the order token names, taken before the transaction.
    let key = token::presented(k, r.ctx.tenant, body.get("token").and_then(Value::as_str)).await?;
    with_retry(|| async {
        let mut tx = k.db.tenant_tx(r.ctx.tenant).await?;
        let store = Store::load(k, &r.ctx, &mut tx).await?;
        let so = match (own, r.user) {
            (Some(number), Some(user)) => orders::own_order(k, &r.ctx, &mut tx, user, number).await?,
            _ => orders::guest_order(k, &r.ctx, &mut tx, text(body, "number")?, text(body, "email")?, text(body, "token")?, key.as_ref()).await?,
        };
        let return_url = checkout::return_url(&store, body.get("return_url"))?;
        let pr = orders::pay_again(k, &r.ctx, &mut tx, &store, &so, &gateway, return_url).await?;
        tx.commit().await?;
        Ok(json!({"payment_request": pr}))
    })
    .await
}

#[async_trait]
impl Site for Storefront {
    /// The storefront's pages at the root of its `site:commerce` host, so a domain can be given to it.
    fn serves_pages(&self) -> bool {
        true
    }

    async fn handle(&self, k: &Kernel, req: SiteRequest) -> Result<SiteResponse> {
        let schema = match &req.user {
            Some(u) => u.schema.clone(),
            None => k.registry.schema(req.tenant).await?,
        };
        let mut ctx = Ctx::system(req.tenant, schema);
        ctx.request_id = format!("commerce-{}", Uuid::now_v7());
        // The storefront's own pages, on a host of its own.
        if crate::pages::is_page(&req) {
            return crate::pages::handle(k, &ctx, &req).await;
        }
        let r = Req { raw: &req, ctx, user: req.user.as_ref() };
        let (status, body) = route(k, &r).await?;
        let mut resp = SiteResponse::json(status, &body);
        resp.headers.push(("cache-control".into(), "no-store".into()));
        Ok(resp)
    }
}
