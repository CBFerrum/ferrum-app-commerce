//! Shopping carts: sales-shaped documents the hook prices on every change with the selling
//! engine (`pricing::calculate`: price list, pricing rules incl. coupon rules, taxes, totals), so
//! the client never sets a price. Shipping is a service line priced by the chosen rule.
//! Cart operations used by the storefront live here too; they take the cart row locked.

use crate::catalog::{self, available, stock_items, stock_view};
use crate::coupon;
use crate::shipping::{Rule, enabled_rules};
use crate::store::{Store, valid_country};
use crate::token;
use async_trait::async_trait;
use ferrum_app_erp::pricing::{Side, calculate};
use ferrum_domain::util::{b, d, get, id, money, round, rows, s, set};
use ferrum_kernel::{Ctx, Doc, DocHook, Error, FieldError, HookCtx, Kernel, Result, Tx};
use ferrum_meta::EffEntity;
use rust_decimal::Decimal;
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use uuid::Uuid;

const PRICE_FIELDS: [&str; 7] = ["price_list_rate", "discount_percent", "rate", "amount", "net_amount", "base_net_amount", "item_tax_rates"];
const ADDRESS_PARTS: [&str; 6] = ["line1", "line2", "city", "state", "postal_code", "country"];

/// Goods lines (everything but the shipping charge).
pub fn goods(cart: &Doc) -> Vec<Doc> {
    rows(cart, "items").into_iter().filter(|r| !b(r, "is_shipping")).collect()
}

fn address_field(prefix: &str, part: &str) -> String {
    match part {
        "line1" | "line2" => format!("{prefix}_address_{part}"),
        other => format!("{prefix}_{other}"),
    }
}

/// Multi-line postal form of the cart's shipping (`ship`) or billing (`bill`) address.
pub fn address_text(cart: &Doc, prefix: &str) -> Option<String> {
    let prefix = if prefix == "bill" && b(cart, "billing_same_as_shipping") { "ship" } else { prefix };
    let part = |p: &str| s(cart, &address_field(prefix, p));
    part("line1")?;
    let mut locality = [part("city"), part("state")].into_iter().flatten().collect::<Vec<_>>().join(", ");
    if let Some(pc) = part("postal_code") {
        if !locality.is_empty() {
            locality.push(' ');
        }
        locality.push_str(pc);
    }
    let lines = [s(cart, "contact_name"), part("line1"), part("line2"), Some(locality.as_str()).filter(|l| !l.is_empty()), part("country")];
    Some(lines.into_iter().flatten().collect::<Vec<_>>().join("\n"))
}

fn address_view(cart: &Doc, prefix: &str) -> Value {
    let mut m = Map::new();
    for p in ADDRESS_PARTS {
        m.insert(p.into(), json!(s(cart, &address_field(prefix, p))));
    }
    Value::Object(m)
}

/// What checkout still needs, as field names of the storefront API.
pub fn missing(cart: &Doc, shipping_rules_exist: bool) -> Vec<&'static str> {
    let mut out = Vec::new();
    if goods(cart).is_empty() {
        out.push("items");
    }
    if s(cart, "email").is_none() {
        out.push("email");
    }
    if s(cart, "contact_name").is_none() {
        out.push("name");
    }
    let complete = |prefix: &str| ["line1", "city", "country"].iter().all(|p| s(cart, &address_field(prefix, p)).is_some());
    if !complete("ship") {
        out.push("shipping_address");
    }
    if !b(cart, "billing_same_as_shipping") && !complete("bill") {
        out.push("billing_address");
    }
    if shipping_rules_exist && id(cart, "shipping_rule").is_none() {
        out.push("shipping_method");
    }
    out
}

pub async fn shipping_rules_exist(tx: &mut Tx, tenant: Uuid) -> Result<bool> {
    Ok(sqlx::query_scalar("select exists(select 1 from e_shipping_rule where tenant_id = $1 and enabled)").bind(tenant).fetch_one(&mut **tx).await?)
}

// ───────────────────────────── the hook ─────────────────────────────

pub struct CartHook;

#[async_trait]
impl DocHook for CartHook {
    async fn before_validate(&self, hc: &mut HookCtx<'_>, doc: &mut Doc) -> Result<()> {
        match s(doc, "status") {
            // Checked-out and expired carts are frozen; `validate` refuses the change.
            Some("Ordered" | "Expired") => return Ok(()),
            Some("Abandoned") => set(doc, "status", "Open"),
            _ => {}
        }
        let sctx = ferrum_domain::util::sys(hc.ctx);
        recalc(hc.kernel, &sctx, hc.tx, hc.entity, doc).await
    }

    async fn validate(&self, _hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        match s(doc, "status") {
            Some("Ordered") => Err(Error::Conflict("this cart has been checked out; start a new cart".into())),
            Some("Expired") => Err(Error::Conflict("this cart has expired; start a new cart".into())),
            _ => Ok(()),
        }
    }
}

fn zero_totals(doc: &mut Doc) {
    for f in
        ["total", "total_discount", "net_total", "total_taxes", "grand_total", "rounding_adjustment", "rounded_total", "base_grand_total", "shipping_amount"]
    {
        set(doc, f, money(Decimal::ZERO));
    }
    set(doc, "taxes", Value::Array(vec![]));
}

fn clear_coupon(doc: &mut Doc) {
    for f in ["coupon", "coupon_code", "coupon_rule"] {
        set(doc, f, Value::Null);
    }
}

fn reset_prices(doc: &mut Doc) {
    let lines: Vec<Value> = goods(doc)
        .into_iter()
        .map(|mut l| {
            for f in PRICE_FIELDS {
                l.remove(f);
            }
            Value::Object(l)
        })
        .collect();
    set(doc, "items", Value::Array(lines));
    set(doc, "taxes", Value::Array(vec![]));
}

/// Prices the cart from scratch: store defaults, goods lines through the pricing engine, the
/// coupon (dropped when it lapsed), the shipping line, and the cart totals.
pub async fn recalc(k: &Kernel, ctx: &Ctx, tx: &mut Tx, ent: &EffEntity, doc: &mut Doc) -> Result<()> {
    let store = Store::load(k, ctx, tx).await?;
    let today = crate::today().to_string();
    set(doc, "company", store.company.to_string());
    set(doc, "currency", store.currency.to_string());
    set(doc, "price_list", store.price_list.to_string());
    set(doc, "tax_template", json!(store.tax_template.map(|t| t.to_string())));
    set(doc, "posting_date", today.clone());
    set(doc, "conversion_rate", Value::Null);
    set(doc, "last_activity", crate::now_str());

    // Weights from the website items, refreshed on every change.
    let mut lines = goods(doc);
    let wis: Vec<Uuid> = lines.iter().filter_map(|l| id(l, "website_item")).collect();
    let weights: HashMap<Uuid, Decimal> =
        sqlx::query_as::<_, (Uuid, Decimal)>("select id, coalesce(weight_kg, 0) from e_website_item where tenant_id = $1 and id = any($2)")
            .bind(ctx.tenant)
            .bind(&wis)
            .fetch_all(&mut **tx)
            .await?
            .into_iter()
            .collect();
    let mut weight = Decimal::ZERO;
    for l in lines.iter_mut() {
        let w = id(l, "website_item").and_then(|wi| weights.get(&wi)).copied().unwrap_or_default() * d(l, "qty");
        weight += w;
        set(l, "weight_kg", money(w));
        set(l, "is_shipping", false);
    }
    set(doc, "items", Value::Array(lines.into_iter().map(Value::Object).collect()));
    reset_prices(doc);
    set(doc, "total_weight_kg", money(weight));

    // A coupon that lapsed (disabled, out of its dates) stops applying; limits are checked when
    // it is applied and again at checkout.
    if let Some(cid) = id(doc, "coupon") {
        let c = get(k, ctx, tx, "CouponCode", cid).await?;
        let lapsed = b(&c, "disabled") || s(&c, "valid_from").is_some_and(|f| f > today.as_str()) || s(&c, "valid_upto").is_some_and(|u| u < today.as_str());
        if lapsed {
            clear_coupon(doc);
        }
    }

    if goods(doc).is_empty() {
        zero_totals(doc);
        set(doc, "item_count", money(Decimal::ZERO));
        return Ok(());
    }
    calculate(k, ctx, tx, ent, doc, Side::Selling).await?;
    if let Some(cid) = id(doc, "coupon") {
        let min = d(&get(k, ctx, tx, "CouponCode", cid).await?, "min_order_amount");
        if min > Decimal::ZERO && list_subtotal(doc, store.precision) < min {
            clear_coupon(doc);
            reset_prices(doc);
            calculate(k, ctx, tx, ent, doc, Side::Selling).await?;
        }
    }

    // Shipping: the chosen rule if it still applies, else the only one that does.
    let subtotal: Decimal = goods(doc).iter().map(|l| d(l, "amount")).sum();
    let rules = enabled_rules(k, ctx, tx).await?;
    let country = s(doc, "ship_country").map(str::to_string);
    let applicable: Vec<(&Rule, Decimal)> = rules.iter().filter_map(|r| r.charge(country.as_deref(), subtotal, weight).map(|c| (r, c))).collect();
    let chosen =
        id(doc, "shipping_rule").and_then(|rid| applicable.iter().find(|(r, _)| r.id == rid)).or(if applicable.len() == 1 { applicable.first() } else { None });
    set(doc, "shipping_rule", json!(chosen.map(|(r, _)| r.id.to_string())));
    let charge = chosen.map(|(_, c)| *c).unwrap_or_default();
    set(doc, "shipping_amount", money(charge));
    if charge > Decimal::ZERO {
        let item = store.shipping_item.ok_or_else(|| Error::Unavailable("shipping charges need the Shipping Item in Store Settings".into()))?;
        let mut items = rows(doc, "items");
        items.push(
            json!({"id": Uuid::now_v7().to_string(), "item": item.to_string(), "qty": "1", "price_list_rate": money(charge), "discount_percent": "0",
                   "is_shipping": true, "weight_kg": "0"})
            .as_object()
            .cloned()
            .unwrap_or_default(),
        );
        set(doc, "items", Value::Array(items.into_iter().map(Value::Object).collect()));
        calculate(k, ctx, tx, ent, doc, Side::Selling).await?;
    }

    let lines = goods(doc);
    set(doc, "item_count", money(lines.iter().map(|l| d(l, "qty")).sum()));
    let charged: Decimal = lines.iter().map(|l| d(l, "amount")).sum();
    set(doc, "total_discount", money((list_subtotal(doc, store.precision) - charged).max(Decimal::ZERO)));
    Ok(())
}

/// Goods at list prices (before discounts), rounded per line like amounts are.
pub fn list_subtotal(cart: &Doc, precision: u32) -> Decimal {
    goods(cart).iter().map(|l| round(d(l, "price_list_rate") * d(l, "qty"), precision)).sum()
}

// ───────────────────────────── operations ─────────────────────────────

/// A new empty cart and its token (shown once; only its hash is kept).
pub async fn create(k: &Kernel, ctx: &Ctx, tx: &mut Tx, user_email: Option<&str>) -> Result<(String, Doc)> {
    let (token, hash) = token::new_cart_token();
    let mut doc = Doc::new();
    set(&mut doc, "token_hash", hash);
    set(&mut doc, "user_email", json!(user_email));
    set(&mut doc, "items", Value::Array(vec![]));
    let cart = k.insert_tx(ctx, tx, "ShoppingCart", doc).await?;
    Ok((token, cart))
}

/// The cart behind a token, locked for this transaction.
pub async fn locate(k: &Kernel, ctx: &Ctx, tx: &mut Tx, cart_token: &str) -> Result<Doc> {
    if !token::well_formed(cart_token) {
        return Err(Error::NotFound("cart".into()));
    }
    let cid: Option<Uuid> = sqlx::query_scalar("select id from e_shopping_cart where tenant_id = $1 and token_hash = $2")
        .bind(ctx.tenant)
        .bind(token::hash(cart_token))
        .fetch_optional(&mut **tx)
        .await?;
    let cid = cid.ok_or_else(|| Error::NotFound("cart".into()))?;
    k.get_tx(ctx, tx, "ShoppingCart", cid, true).await
}

fn cart_id(cart: &Doc) -> Result<Uuid> {
    id(cart, "id").ok_or_else(|| Error::internal("cart without id"))
}

/// Rewrites kernel field errors in the storefront's terms (`ship_city` → `shipping_address.city`).
pub fn public_errors(e: Error) -> Error {
    match e {
        Error::Validation(fields) => Error::Validation(
            fields
                .into_iter()
                .map(|mut f| {
                    for (prefix, name) in [("ship", "shipping_address"), ("bill", "billing_address")] {
                        if let Some(part) = ADDRESS_PARTS.iter().find(|p| address_field(prefix, p) == f.field) {
                            f.field = format!("{name}.{part}");
                        }
                    }
                    if f.field == "contact_name" {
                        f.field = "name".into();
                    }
                    f.row = None;
                    f
                })
                .collect(),
        ),
        other => other,
    }
}

async fn save(k: &Kernel, ctx: &Ctx, tx: &mut Tx, cart: &Doc, patch: Doc) -> Result<Doc> {
    k.update_tx(ctx, tx, "ShoppingCart", cart_id(cart)?, patch, None).await.map_err(public_errors)
}

fn items_patch(lines: &[Doc]) -> Value {
    Value::Array(
        lines
            .iter()
            .map(|l| match s(l, "id") {
                Some(lid) => json!({"id": lid, "qty": l.get("qty")}),
                None => json!({"item": l.get("item"), "website_item": l.get("website_item"), "qty": l.get("qty")}),
            })
            .collect(),
    )
}

/// The website item behind a storefront reference (slug, or id), published with an enabled item.
pub async fn website_item(k: &Kernel, ctx: &Ctx, tx: &mut Tx, reference: &str) -> Result<Doc> {
    let unavailable = || Error::field("item", "this item is not available");
    let slug = match Uuid::parse_str(reference.trim()) {
        Ok(wid) => match get(k, ctx, tx, "WebsiteItem", wid).await {
            Ok(wi) => s(&wi, "slug").map(str::to_string).ok_or_else(unavailable)?,
            Err(Error::NotFound(_)) => return Err(unavailable()),
            Err(e) => return Err(e),
        },
        Err(_) => reference.trim().to_string(),
    };
    match catalog::published_by_slug(k, ctx, tx, &slug).await {
        Err(Error::NotFound(_)) => Err(unavailable()),
        other => other,
    }
}

/// List price of an item for the store today (price list, else the item's standard rate).
async fn has_price(tx: &mut Tx, tenant: Uuid, store: &Store, item: Uuid) -> Result<bool> {
    let price: Option<Option<Decimal>> = sqlx::query_scalar(
        "select coalesce((select p.rate from e_item_price p where p.tenant_id = i.tenant_id and p.item = i.id and p.price_list = $3 \
           and p.valid_from <= $4 and (p.valid_upto is null or p.valid_upto >= $4) order by p.valid_from desc limit 1), nullif(i.standard_rate, 0)) \
         from e_item i where i.tenant_id = $1 and i.id = $2",
    )
    .bind(tenant)
    .bind(item)
    .bind(store.price_list)
    .bind(crate::today())
    .fetch_optional(&mut **tx)
    .await?;
    Ok(price.flatten().is_some_and(|p| p > Decimal::ZERO))
}

/// Refuses quantities that are not positive, above the item's or store's limit, or fractional
/// for a whole-number unit.
pub async fn check_qty(k: &Kernel, ctx: &Ctx, tx: &mut Tx, store: &Store, wi: &Doc, qty: Decimal) -> Result<()> {
    if qty <= Decimal::ZERO {
        return Err(Error::field("qty", "must be greater than zero"));
    }
    let own = d(wi, "max_qty");
    let max = if own > Decimal::ZERO { own.min(store.max_qty_per_line) } else { store.max_qty_per_line };
    if qty > max {
        return Err(Error::field("qty", format!("at most {} per order", max.normalize())));
    }
    let item = get(k, ctx, tx, "Item", id(wi, "item").ok_or_else(|| Error::internal("website item without item"))?).await?;
    if let Some(uom) = id(&item, "stock_uom")
        && b(&get(k, ctx, tx, "Uom", uom).await?, "whole_number")
        && !qty.fract().is_zero()
    {
        return Err(Error::field("qty", "must be a whole number"));
    }
    Ok(())
}

/// Refuses a quantity of a stock item beyond what the store can sell (when it enforces stock).
pub async fn check_stock(tx: &mut Tx, tenant: Uuid, store: &Store, item: Uuid, qty: Decimal) -> Result<()> {
    if !store.enforce_stock || !stock_items(tx, tenant, &[item]).await?.contains(&item) {
        return Ok(());
    }
    let avail = available(tx, tenant, store, &[item]).await?.get(&item).copied().unwrap_or_default().max(Decimal::ZERO);
    if qty <= avail {
        return Ok(());
    }
    Err(Error::field(
        "qty",
        if avail.is_zero() {
            "this item is out of stock".to_string()
        } else if store.show_stock_quantity {
            format!("only {} available", avail.normalize())
        } else {
            "not enough stock for this quantity".to_string()
        },
    ))
}

pub async fn add_item(k: &Kernel, ctx: &Ctx, tx: &mut Tx, store: &Store, cart: &Doc, reference: &str, qty: Decimal) -> Result<Doc> {
    let wi = website_item(k, ctx, tx, reference).await?;
    let item = id(&wi, "item").ok_or_else(|| Error::internal("website item without item"))?;
    if !has_price(tx, ctx.tenant, store, item).await? {
        return Err(Error::field("item", "this item is not for sale"));
    }
    let mut lines = goods(cart);
    let total = match lines.iter_mut().find(|l| id(l, "item") == Some(item)) {
        Some(line) => {
            let q = d(line, "qty") + qty;
            set(line, "qty", money(q));
            q
        }
        None => {
            if lines.len() >= store.max_cart_lines {
                return Err(Error::field("items", format!("a cart holds at most {} different items", store.max_cart_lines)));
            }
            lines.push(json!({"item": item.to_string(), "website_item": s(&wi, "id"), "qty": money(qty)}).as_object().cloned().unwrap_or_default());
            qty
        }
    };
    check_qty(k, ctx, tx, store, &wi, qty).await?;
    check_qty(k, ctx, tx, store, &wi, total).await?;
    check_stock(tx, ctx.tenant, store, item, total).await?;
    let mut patch = Doc::new();
    patch.insert("items".into(), items_patch(&lines));
    save(k, ctx, tx, cart, patch).await
}

/// Sets a line's quantity (0 removes it).
pub async fn set_qty(k: &Kernel, ctx: &Ctx, tx: &mut Tx, store: &Store, cart: &Doc, line: &str, qty: Decimal) -> Result<Doc> {
    let mut lines = goods(cart);
    let pos = lines.iter().position(|l| s(l, "id") == Some(line)).ok_or_else(|| Error::NotFound("cart line".into()))?;
    if qty.is_zero() {
        lines.remove(pos);
    } else {
        let wid = id(&lines[pos], "website_item").ok_or_else(|| Error::field("item", "this item is not available"))?;
        let wi = website_item(k, ctx, tx, &wid.to_string()).await?;
        check_qty(k, ctx, tx, store, &wi, qty).await?;
        let item = id(&lines[pos], "item").ok_or_else(|| Error::internal("cart line without item"))?;
        check_stock(tx, ctx.tenant, store, item, qty).await?;
        set(&mut lines[pos], "qty", money(qty));
    }
    let mut patch = Doc::new();
    patch.insert("items".into(), items_patch(&lines));
    save(k, ctx, tx, cart, patch).await
}

fn text(v: &Value, field: &str, max: usize) -> Result<Value> {
    match v {
        Value::Null => Ok(Value::Null),
        Value::String(t) => {
            let t = t.trim();
            if t.chars().count() > max {
                return Err(Error::field(field, format!("at most {max} characters")));
            }
            Ok(if t.is_empty() { Value::Null } else { json!(t) })
        }
        _ => Err(Error::field(field, "must be text")),
    }
}

/// Contact details and addresses. Keys left out stay as they are; an address replaces the
/// previous one whole.
pub async fn set_details(k: &Kernel, ctx: &Ctx, tx: &mut Tx, store: &Store, cart: &Doc, body: &Map<String, Value>) -> Result<Doc> {
    let mut patch = Doc::new();
    for (key, v) in body {
        match key.as_str() {
            "email" => {
                patch.insert("email".into(), text(v, "email", 254)?);
            }
            "phone" => {
                patch.insert("phone".into(), text(v, "phone", 40)?);
            }
            "name" => {
                patch.insert("contact_name".into(), text(v, "name", 140)?);
            }
            "notes" => {
                patch.insert("notes".into(), text(v, "notes", 2000)?);
            }
            "billing_same_as_shipping" => {
                patch.insert("billing_same_as_shipping".into(), json!(v.as_bool().ok_or_else(|| Error::field(key, "must be true or false"))?));
            }
            "shipping_address" | "billing_address" => {
                let (prefix, name) = if key == "shipping_address" { ("ship", "shipping_address") } else { ("bill", "billing_address") };
                let addr = match v {
                    Value::Null => Map::new(),
                    Value::Object(m) => m.clone(),
                    _ => return Err(Error::field(name, "must be an object")),
                };
                if let Some(unknown) = addr.keys().find(|k| !ADDRESS_PARTS.contains(&k.as_str())) {
                    return Err(Error::field(format!("{name}.{unknown}"), "unknown field"));
                }
                for part in ADDRESS_PARTS {
                    let mut value = text(addr.get(part).unwrap_or(&Value::Null), &format!("{name}.{part}"), 200)?;
                    if part == "country"
                        && let Value::String(c) = &value
                    {
                        let code = c.to_uppercase();
                        if !valid_country(&code) {
                            return Err(Error::field(format!("{name}.country"), "use a two-letter ISO country code"));
                        }
                        if prefix == "ship" && !store.ships_to(&code) {
                            return Err(Error::field(format!("{name}.country"), "we do not ship to this country"));
                        }
                        value = json!(code);
                    }
                    patch.insert(address_field(prefix, part), value);
                }
                if prefix == "bill" && !body.contains_key("billing_same_as_shipping") {
                    patch.insert("billing_same_as_shipping".into(), json!(v.is_null()));
                }
            }
            other => return Err(Error::field(other, "unknown field")),
        }
    }
    save(k, ctx, tx, cart, patch).await
}

pub async fn apply_coupon(k: &Kernel, ctx: &Ctx, tx: &mut Tx, store: &Store, cart: &Doc, code: &str) -> Result<Doc> {
    let code = coupon::normalize_code(code);
    let unknown = || Error::field("coupon_code", "this coupon code is not valid");
    if !coupon::valid_code(&code) {
        return Err(unknown());
    }
    let c = coupon::by_code(k, ctx, tx, &code).await?.ok_or_else(unknown)?;
    let rule = id(&c, "pricing_rule").ok_or_else(unknown)?;
    let today = crate::today().to_string();
    coupon::check(tx, ctx.tenant, &c, list_subtotal(cart, store.precision), id(cart, "customer"), s(cart, "email"), cart_id(cart)?, &today).await?;
    let before: Decimal = goods(cart).iter().map(|l| d(l, "amount")).sum();
    let mut patch = Doc::new();
    patch.insert("coupon".into(), json!(s(&c, "id")));
    patch.insert("coupon_code".into(), json!(code));
    patch.insert("coupon_rule".into(), json!(rule.to_string()));
    let updated = save(k, ctx, tx, cart, patch).await?;
    let after: Decimal = goods(&updated).iter().map(|l| d(l, "amount")).sum();
    if id(&updated, "coupon").is_none() || after >= before {
        return Err(Error::field("coupon_code", "this coupon does not apply to the items in your cart"));
    }
    Ok(updated)
}

pub async fn remove_coupon(k: &Kernel, ctx: &Ctx, tx: &mut Tx, cart: &Doc) -> Result<Doc> {
    let mut patch = Doc::new();
    clear_coupon(&mut patch);
    save(k, ctx, tx, cart, patch).await
}

/// Shipping rules the cart can use now, with their charges.
pub async fn shipping_options(k: &Kernel, ctx: &Ctx, tx: &mut Tx, cart: &Doc) -> Result<Vec<(Rule, Decimal)>> {
    let subtotal: Decimal = goods(cart).iter().map(|l| d(l, "amount")).sum();
    let weight = d(cart, "total_weight_kg");
    let country = s(cart, "ship_country");
    Ok(enabled_rules(k, ctx, tx).await?.into_iter().filter_map(|r| r.charge(country, subtotal, weight).map(|c| (r, c))).collect())
}

pub async fn set_shipping(k: &Kernel, ctx: &Ctx, tx: &mut Tx, cart: &Doc, rule: &str) -> Result<Doc> {
    let wanted = Uuid::parse_str(rule.trim()).map_err(|_| Error::field("shipping_method", "unknown shipping method"))?;
    if !shipping_options(k, ctx, tx, cart).await?.iter().any(|(r, _)| r.id == wanted) {
        return Err(Error::field("shipping_method", "this shipping method is not available for your cart and address"));
    }
    let mut patch = Doc::new();
    patch.insert("shipping_rule".into(), json!(wanted.to_string()));
    save(k, ctx, tx, cart, patch).await
}

/// The cart as the storefront sees it: no internal pricing fields, ids only where the UI needs
/// them (lines, shipping methods), stock as in/out unless the store shows quantities.
pub async fn view(k: &Kernel, ctx: &Ctx, tx: &mut Tx, store: &Store, cart: &Doc) -> Result<Value> {
    let lines = goods(cart);
    let wis: Vec<Uuid> = lines.iter().filter_map(|l| id(l, "website_item")).collect();
    let meta: HashMap<Uuid, (Option<String>, Option<String>, bool)> = sqlx::query_as::<_, (Uuid, Option<String>, Option<String>, bool)>(
        "select w.id, w.slug, w.title, coalesce(w.published, false) and not coalesce(i.disabled, false) from e_website_item w \
         join e_item i on i.tenant_id = w.tenant_id and i.id = w.item where w.tenant_id = $1 and w.id = any($2)",
    )
    .bind(ctx.tenant)
    .bind(&wis)
    .fetch_all(&mut **tx)
    .await?
    .into_iter()
    .map(|(i, slug, title, sold)| (i, (slug, title, sold)))
    .collect();
    let images: HashMap<Uuid, (String, Option<String>)> = sqlx::query_as::<_, (Uuid, String, Option<String>)>(
        "select distinct on (parent_id) parent_id, image, alt_text from e_website_item__images where tenant_id = $1 and parent_id = any($2) order by parent_id, idx",
    )
    .bind(ctx.tenant)
    .bind(&wis)
    .fetch_all(&mut **tx)
    .await?
    .into_iter()
    .map(|(p, url, alt)| (p, (url, alt)))
    .collect();
    let items: Vec<Uuid> = lines.iter().filter_map(|l| id(l, "item")).collect();
    let stock = stock_items(tx, ctx.tenant, &items).await?;
    let stocked: Vec<Uuid> = stock.iter().copied().collect();
    let avail = available(tx, ctx.tenant, store, &stocked).await?;
    let m = |v: Decimal| v.normalize().to_string();
    let line_views: Vec<Value> = lines
        .iter()
        .map(|l| {
            let wi = id(l, "website_item");
            // An item unpublished (or deleted) since it was added: checkout refuses the line.
            let (slug, title, sold) = wi.and_then(|w| meta.get(&w)).cloned().unwrap_or_default();
            let item = id(l, "item").unwrap_or_default();
            let (in_stock, qty) = stock_view(store, stock.contains(&item), avail.get(&item).copied().unwrap_or_default());
            let enough = !stock.contains(&item) || !store.enforce_stock || avail.get(&item).copied().unwrap_or_default() >= d(l, "qty");
            json!({
                "id": s(l, "id"),
                "item": slug,
                "title": title.or_else(|| s(l, "item_name").map(str::to_string)),
                "image": wi.and_then(|w| images.get(&w)).map(|(u, a)| json!({"url": u, "alt": a})),
                "qty": m(d(l, "qty")),
                "list_price": m(d(l, "price_list_rate")),
                "price": m(d(l, "rate")),
                "discount_percent": m(d(l, "discount_percent")),
                "amount": m(d(l, "amount")),
                "available": sold,
                "in_stock": in_stock && enough,
                "available_qty": qty,
            })
        })
        .collect();
    let shipping = match id(cart, "shipping_rule") {
        Some(r) => {
            let rule = get(k, ctx, tx, "ShippingRule", r).await?;
            json!({"id": r, "label": s(&rule, "label"), "amount": m(d(cart, "shipping_amount"))})
        }
        None => Value::Null,
    };
    let taxes: Vec<Value> = rows(cart, "taxes")
        .iter()
        .filter(|t| !d(t, "tax_amount").is_zero())
        .map(|t| json!({"description": s(t, "description"), "amount": m(if b(t, "deduct") { -d(t, "tax_amount") } else { d(t, "tax_amount") })}))
        .collect();
    let subtotal: Decimal = lines.iter().map(|l| d(l, "amount")).sum();
    let rules_exist = shipping_rules_exist(tx, ctx.tenant).await?;
    let status = match s(cart, "status") {
        Some("Abandoned") | None => "Open",
        Some(other) => other,
    };
    Ok(json!({
        "status": status,
        "currency": store.currency_code,
        "items": line_views,
        "item_count": m(d(cart, "item_count")),
        "coupon": s(cart, "coupon_code").map(|c| json!({"code": c})),
        "shipping": shipping,
        "email": s(cart, "email"),
        "phone": s(cart, "phone"),
        "name": s(cart, "contact_name"),
        "notes": s(cart, "notes"),
        "shipping_address": address_view(cart, "ship"),
        "billing_same_as_shipping": b(cart, "billing_same_as_shipping"),
        "billing_address": address_view(cart, "bill"),
        "prices_include_tax": rows(cart, "taxes").iter().any(|t| b(t, "included_in_price")),
        "totals": {
            "subtotal": m(subtotal),
            "discount": m(d(cart, "total_discount")),
            "shipping": m(d(cart, "shipping_amount")),
            "taxes": taxes,
            "total_taxes": m(d(cart, "total_taxes")),
            "grand_total": m(d(cart, "grand_total")),
            "rounding_adjustment": m(d(cart, "rounding_adjustment")),
            "rounded_total": m(d(cart, "rounded_total")),
        },
        "missing": missing(cart, rules_exist),
    }))
}

/// Field errors naming what checkout still needs.
pub fn missing_error(missing: &[&str]) -> Error {
    Error::Validation(
        missing
            .iter()
            .map(|m| {
                FieldError::new(
                    *m,
                    match *m {
                        "items" => "your cart is empty",
                        "shipping_method" => "choose a shipping method available for your address",
                        _ => "is required to check out",
                    },
                )
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(v: Value) -> Doc {
        v.as_object().cloned().unwrap()
    }

    #[test]
    fn addresses_and_missing_details() {
        let mut cart = doc(json!({"contact_name": "Asha Rao", "ship_address_line1": "12 MG Road", "ship_city": "Bengaluru", "ship_state": "KA",
                                   "ship_postal_code": "560001", "ship_country": "IN", "billing_same_as_shipping": true,
                                   "items": [{"id": "a", "item": "x", "qty": "1"}, {"id": "s", "item": "ship", "is_shipping": true}]}));
        assert_eq!(address_text(&cart, "ship").unwrap(), "Asha Rao\n12 MG Road\nBengaluru, KA 560001\nIN");
        assert_eq!(address_text(&cart, "bill"), address_text(&cart, "ship"), "billing follows shipping");
        assert_eq!(goods(&cart).len(), 1, "the shipping line is not goods");
        assert_eq!(missing(&cart, true), vec!["email", "shipping_method"]);
        cart.insert("billing_same_as_shipping".into(), json!(false));
        cart.insert("email".into(), json!("a@x.in"));
        assert_eq!(missing(&cart, false), vec!["billing_address"]);
    }

    #[test]
    fn kernel_errors_speak_storefront() {
        let e = public_errors(Error::Validation(vec![FieldError::new("ship_city", "too long"), FieldError::new("contact_name", "x")]));
        let Error::Validation(f) = e else { panic!() };
        assert_eq!(f[0].field, "shipping_address.city");
        assert_eq!(f[1].field, "name");
    }
}
