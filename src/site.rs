//! The storefront API: `/api/site/{tenant}/commerce/...` for the public and
//! `/api/app/commerce/...` for signed-in customers (same routes, plus their orders and invoices).
//! Every request acts with the system context after validating its input: callers choose items,
//! quantities and details — never prices, totals, customers or statuses. Carts are addressed by
//! the `X-Cart-Token` header, guest orders by number + email + the `X-Order-Token` from checkout.

use crate::catalog::{self, Listing, Sort, decimal_param};
use crate::checkout::{self, Checkout};
use crate::store::Store;
use crate::{cart, orders};
use async_trait::async_trait;
use ferrum_domain::util::{id, s};
use ferrum_kernel::{Ctx, Doc, Error, Kernel, Result, Site, SiteRequest, SiteResponse, Tx, with_retry};
use rust_decimal::Decimal;
use serde_json::{Map, Value, json};
use std::str::FromStr;
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

fn qty(body: &Map<String, Value>, default: Option<Decimal>) -> Result<Decimal> {
    let bad = || Error::field("qty", "must be a number");
    match body.get("qty") {
        None | Some(Value::Null) => default.ok_or_else(|| Error::field("qty", "is required")),
        Some(Value::String(v)) => Decimal::from_str(v.trim()).map_err(|_| bad()),
        Some(Value::Number(n)) => Decimal::from_str(&n.to_string()).map_err(|_| bad()),
        Some(_) => Err(bad()),
    }
    .map(|q| q.normalize())
}

/// The signed-in caller's customer (for their prices), if any.
async fn customer_of(k: &Kernel, r: &Req<'_>, tx: &mut Tx) -> Result<Option<Uuid>> {
    match r.user {
        Some(u) => Ok(orders::linked_customers(k, &r.ctx, tx, &u.email).await?.into_iter().next()),
        None => Ok(None),
    }
}

enum CartOp {
    View,
    Add(String, Decimal),
    SetQty(String, Decimal),
    Details(Map<String, Value>),
    Coupon(String),
    NoCoupon,
    Shipping(String),
}

async fn cart_op(k: &Kernel, r: &Req<'_>, op: CartOp) -> Result<Value> {
    let token = r.cart_token()?;
    with_retry(|| async {
        let mut tx = k.db.tenant_tx(r.ctx.tenant).await?;
        let store = Store::load(k, &r.ctx, &mut tx).await?;
        let mut cart = cart::locate(k, &r.ctx, &mut tx, token).await?;
        if !matches!(op, CartOp::View) {
            cart = attach_user(k, r, &mut tx, cart).await?;
        }
        let cart = match &op {
            CartOp::View => cart,
            CartOp::Add(item, q) => cart::add_item(k, &r.ctx, &mut tx, &store, &cart, item, *q).await?,
            CartOp::SetQty(line, q) => cart::set_qty(k, &r.ctx, &mut tx, &store, &cart, line, *q).await?,
            CartOp::Details(body) => cart::set_details(k, &r.ctx, &mut tx, &store, &cart, body).await?,
            CartOp::Coupon(code) => cart::apply_coupon(k, &r.ctx, &mut tx, &store, &cart, code).await?,
            CartOp::NoCoupon => cart::remove_coupon(k, &r.ctx, &mut tx, &cart).await?,
            CartOp::Shipping(rule) => cart::set_shipping(k, &r.ctx, &mut tx, &cart, rule).await?,
        };
        let view = cart::view(k, &r.ctx, &mut tx, &store, &cart).await?;
        tx.commit().await?;
        Ok(view)
    })
    .await
}

/// A signed-in customer's cart carries their customer, so it shows their prices.
async fn attach_user(k: &Kernel, r: &Req<'_>, tx: &mut Tx, cart: Doc) -> Result<Doc> {
    let Some(u) = r.user else { return Ok(cart) };
    let email = u.email.to_lowercase();
    let customer = if id(&cart, "customer").is_none() { orders::linked_customers(k, &r.ctx, tx, &email).await?.into_iter().next() } else { None };
    if customer.is_none() && s(&cart, "user_email") == Some(email.as_str()) {
        return Ok(cart);
    }
    let mut patch = Doc::new();
    patch.insert("user_email".into(), json!(email));
    if let Some(c) = customer {
        patch.insert("customer".into(), json!(c.to_string()));
    }
    let cid = id(&cart, "id").ok_or_else(|| Error::internal("cart without id"))?;
    k.update_tx(&r.ctx, tx, "ShoppingCart", cid, patch, None).await
}

async fn create_cart(k: &Kernel, r: &Req<'_>) -> Result<Value> {
    let body = r.body()?;
    let lines: Vec<(String, Decimal)> = match body.get("items") {
        None | Some(Value::Null) => vec![],
        Some(Value::Array(a)) if a.len() <= 100 => a
            .iter()
            .map(|l| {
                let m = l.as_object().ok_or_else(|| Error::field("items", "each item is an object"))?;
                Ok((text(m, "item")?.to_string(), qty(m, Some(Decimal::ONE))?))
            })
            .collect::<Result<_>>()?,
        Some(_) => return Err(Error::field("items", "must be a list of at most 100 items")),
    };
    with_retry(|| async {
        let mut tx = k.db.tenant_tx(r.ctx.tenant).await?;
        let store = Store::load(k, &r.ctx, &mut tx).await?;
        let (token, mut cart) = cart::create(k, &r.ctx, &mut tx, r.user.map(|u| u.email.to_lowercase()).as_deref()).await?;
        cart = attach_user(k, r, &mut tx, cart).await?;
        for (item, q) in &lines {
            cart = cart::add_item(k, &r.ctx, &mut tx, &store, &cart, item, *q).await?;
        }
        let view = cart::view(k, &r.ctx, &mut tx, &store, &cart).await?;
        tx.commit().await?;
        Ok(json!({"token": token, "cart": view}))
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
    let store = Store::load(k, &r.ctx, &mut tx).await?;
    let customer = customer_of(k, r, &mut tx).await?;
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
            let out = with_retry(|| async {
                let mut tx = k.db.tenant_tx(r.ctx.tenant).await?;
                let out = catalog::submit_review(k, &r.ctx, &mut tx, slug, &body, r.user).await?;
                tx.commit().await?;
                Ok(out)
            })
            .await?;
            Ok((201, out))
        }
        ("POST", ["cart"]) => Ok((201, create_cart(k, r).await?)),
        ("GET", ["cart"]) => ok(cart_op(k, r, CartOp::View).await?),
        ("POST", ["cart", "items"]) => {
            let body = r.body()?;
            ok(cart_op(k, r, CartOp::Add(text(&body, "item")?.to_string(), qty(&body, Some(Decimal::ONE))?)).await?)
        }
        ("PUT" | "PATCH", ["cart", "items", line]) => ok(cart_op(k, r, CartOp::SetQty(line.to_string(), qty(&r.body()?, None)?)).await?),
        ("DELETE", ["cart", "items", line]) => ok(cart_op(k, r, CartOp::SetQty(line.to_string(), Decimal::ZERO)).await?),
        ("PUT" | "PATCH", ["cart", "details"]) => ok(cart_op(k, r, CartOp::Details(r.body()?)).await?),
        ("PUT", ["cart", "coupon"]) => ok(cart_op(k, r, CartOp::Coupon(text(&r.body()?, "code")?.to_string())).await?),
        ("DELETE", ["cart", "coupon"]) => ok(cart_op(k, r, CartOp::NoCoupon).await?),
        ("GET", ["cart", "shipping-options"]) => ok(shipping_options(k, r).await?),
        ("PUT", ["cart", "shipping"]) => ok(cart_op(k, r, CartOp::Shipping(text(&r.body()?, "shipping_method")?.to_string())).await?),
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
        ("GET", ["orders"]) => {
            let user = r.signed_in()?;
            ok(orders::own_documents(k, &r.ctx, user, "SalesOrder", r.query("cursor").map(str::to_string)).await?)
        }
        ("GET", ["invoices"]) => {
            let user = r.signed_in()?;
            ok(orders::own_documents(k, &r.ctx, user, "SalesInvoice", r.query("cursor").map(str::to_string)).await?)
        }
        ("POST", ["orders", number, "pay"]) => {
            r.signed_in()?;
            let body = r.body()?;
            ok(pay(k, r, &body, Some(number)).await?)
        }
        ("POST", ["orders", number, "reorder"]) => {
            let user = r.signed_in()?;
            let out = with_retry(|| async {
                let mut tx = k.db.tenant_tx(r.ctx.tenant).await?;
                let store = Store::load(k, &r.ctx, &mut tx).await?;
                let so = orders::own_order(k, &r.ctx, &mut tx, user, number).await?;
                let (token, cart, skipped) = orders::reorder(k, &r.ctx, &mut tx, &store, user, &so).await?;
                let cart = attach_user(k, r, &mut tx, cart).await?;
                let view = cart::view(k, &r.ctx, &mut tx, &store, &cart).await?;
                tx.commit().await?;
                Ok(json!({"token": token, "cart": view, "skipped": skipped}))
            })
            .await?;
            Ok((201, out))
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
    let mut tx = k.db.tenant_tx(r.ctx.tenant).await?;
    let store = Store::load(k, &r.ctx, &mut tx).await?;
    let out = match (r.raw.method.as_str(), segs) {
        ("GET", ["store"]) => store.view(),
        ("GET", ["categories"]) => json!({"categories": catalog::category_tree(&mut tx, r.ctx.tenant).await?}),
        ("GET", ["items", slug]) => {
            let customer = customer_of(k, r, &mut tx).await?;
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
            orders::request_for_token(k, &r.ctx, &mut tx, &store, request, token).await?
        }
        ("POST", ["orders", "lookup"]) => {
            let body = r.body()?;
            let so = orders::guest_order(k, &r.ctx, &mut tx, text(&body, "number")?, text(&body, "email")?, text(&body, "token")?).await?;
            json!({"order": orders::order_view(k, &r.ctx, &mut tx, &store, &so).await?})
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

/// Pays an unpaid order again: guests identify it like a lookup, signed-in customers by number.
async fn pay(k: &Kernel, r: &Req<'_>, body: &Map<String, Value>, own: Option<&str>) -> Result<Value> {
    let gateway = text(body, "payment_method")?.to_string();
    with_retry(|| async {
        let mut tx = k.db.tenant_tx(r.ctx.tenant).await?;
        let store = Store::load(k, &r.ctx, &mut tx).await?;
        let so = match (own, r.user) {
            (Some(number), Some(user)) => orders::own_order(k, &r.ctx, &mut tx, user, number).await?,
            _ => orders::guest_order(k, &r.ctx, &mut tx, text(body, "number")?, text(body, "email")?, text(body, "token")?).await?,
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
    async fn handle(&self, k: &Kernel, req: SiteRequest) -> Result<SiteResponse> {
        let schema = match &req.user {
            Some(u) => u.schema.clone(),
            None => k.registry.schema(req.tenant).await?,
        };
        let mut ctx = Ctx::system(req.tenant, schema);
        ctx.request_id = format!("commerce-{}", Uuid::now_v7());
        let r = Req { raw: &req, ctx, user: req.user.as_ref() };
        let (status, body) = route(k, &r).await?;
        let mut resp = SiteResponse::json(status, &body);
        resp.headers.push(("cache-control".into(), "no-store".into()));
        Ok(resp)
    }
}
