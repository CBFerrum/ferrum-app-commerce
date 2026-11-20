//! The catalog: item categories (a tree), website items (the published face of an item) and
//! their moderated reviews, plus the public listing — prices from the pricing engine, stock
//! availability net of open orders, search, filters and keyset pagination.

use crate::store::Store;
use async_trait::async_trait;
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ferrum_app_erp::pricing::{self, Side, calculate};
use ferrum_calc::{CalcInput, ChargeType, LineIn, Rounding, TaxIn};
use ferrum_domain::util::{b, d, find, get, id, rows, s, set, sys};
use ferrum_kernel::{Ctx, Doc, DocHook, Error, FieldError, Filter, HookCtx, Kernel, Result, Tx};
use rust_decimal::Decimal;
use serde_json::{Map, Value, json};
use sqlx::{Postgres, QueryBuilder};
use std::collections::{HashMap, HashSet};
use std::str::FromStr;
use uuid::Uuid;

pub const MAX_PAGE: i64 = 48;

/// URL name: lower-case letters and digits joined by single dashes, at most 140 characters.
pub fn slugify(raw: &str) -> String {
    let mut out = String::new();
    for c in raw.trim().chars() {
        if c.is_alphanumeric() {
            out.extend(c.to_lowercase());
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    let cut: String = out.chars().take(140).collect();
    cut.trim_end_matches('-').to_string()
}

/// Opaque keyset cursor over string parts.
pub fn encode_cursor(parts: &[String]) -> String {
    URL_SAFE_NO_PAD.encode(Value::from(parts.to_vec()).to_string())
}

pub fn decode_cursor(c: &str) -> Result<Vec<String>> {
    let bad = || Error::invalid("invalid cursor");
    let raw = URL_SAFE_NO_PAD.decode(c.trim()).map_err(|_| bad())?;
    let v: Value = serde_json::from_slice(&raw).map_err(|_| bad())?;
    v.as_array().ok_or_else(bad)?.iter().map(|p| p.as_str().map(str::to_string).ok_or_else(bad)).collect()
}

fn escape_like(q: &str) -> String {
    q.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_")
}

/// The words of a search, at most six, each of which must be found: "mug blue" finds "Blue Mug".
pub(crate) fn search_terms(q: &str) -> Vec<String> {
    q.split_whitespace().take(6).map(str::to_string).collect()
}

async fn slug_taken(k: &Kernel, ctx: &Ctx, tx: &mut Tx, entity: &str, slug: &str, me: Option<Uuid>) -> Result<bool> {
    let found = find(k, ctx, tx, entity, vec![Filter::eq("slug", slug)], None, 2).await?;
    Ok(found.iter().any(|r| id(r, "id") != me))
}

// ───────────────────────────── hooks ─────────────────────────────

pub struct CategoryHook;

#[async_trait]
impl DocHook for CategoryHook {
    async fn before_validate(&self, _hc: &mut HookCtx<'_>, doc: &mut Doc) -> Result<()> {
        let base = s(doc, "slug").or_else(|| s(doc, "category_name")).unwrap_or_default().to_string();
        set(doc, "slug", slugify(&base));
        Ok(())
    }

    async fn validate(&self, hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        let slug = s(doc, "slug").unwrap_or_default();
        if slug.is_empty() {
            return Err(Error::field("slug", "use some letters or digits"));
        }
        if slug_taken(hc.kernel, hc.ctx, hc.tx, "ItemCategory", slug, id(doc, "id")).await? {
            return Err(Error::field("slug", "another category already uses this slug"));
        }
        Ok(())
    }
}

pub struct WebsiteItemHook;

/// What makes one variant different from the others, as (option, value) pairs in the order given.
pub(crate) type Options = Vec<(String, String)>;

/// A variant's default name: its group's name and what sets it apart. "Logo Tee — Red / M".
pub(crate) fn variant_title(parent: &str, options: &Options) -> String {
    let what: Vec<&str> = options.iter().map(|(_, v)| v.as_str()).collect();
    format!("{parent} — {}", what.join(" / "))
}

/// What is wrong with a variant's options next to its siblings', in words: each names an option
/// once, every sibling names the same options, and no two siblings are the same choice.
pub(crate) fn option_problems(options: &Options, siblings: &[Options]) -> Vec<String> {
    let lower = |s: &str| s.trim().to_lowercase();
    let mut out = Vec::new();
    if options.is_empty() {
        return vec!["say what makes this variant different, such as Size M".into()];
    }
    let mut names: Vec<String> = options.iter().map(|(n, _)| lower(n)).collect();
    names.sort();
    if names.windows(2).any(|w| w[0] == w[1]) {
        out.push("an option is named twice".into());
    }
    for sib in siblings {
        let mut theirs: Vec<String> = sib.iter().map(|(n, _)| lower(n)).collect();
        theirs.sort();
        if theirs != names {
            out.push(format!("the other variants differ by {}: name the same options", sib.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>().join(" and ")));
            break;
        }
    }
    let mut mine: Vec<(String, String)> = options.iter().map(|(n, v)| (lower(n), lower(v))).collect();
    mine.sort();
    if siblings.iter().any(|sib| {
        let mut theirs: Vec<(String, String)> = sib.iter().map(|(n, v)| (lower(n), lower(v))).collect();
        theirs.sort();
        theirs == mine
    }) {
        out.push("another variant already has these choices".into());
    }
    out
}

/// The options a document names, tidied: blanks dropped, spaces trimmed.
fn options_of(doc: &Doc) -> Options {
    rows(doc, "options")
        .iter()
        .filter_map(|r| {
            let (o, v) = (s(r, "option")?.trim().to_string(), s(r, "value")?.trim().to_string());
            (!o.is_empty() && !v.is_empty()).then_some((o, v))
        })
        .collect()
}

/// Every other variant of a group, with what sets each apart.
async fn sibling_options(tx: &mut Tx, tenant: Uuid, parent: Uuid, except: Option<Uuid>) -> Result<Vec<Options>> {
    let found: Vec<(Uuid, String, String)> = sqlx::query_as(
        "select w.id, o.\"option\", o.\"value\" from e_website_item w join e_website_item__options o on o.tenant_id = w.tenant_id and o.parent_id = w.id \
         where w.tenant_id = $1 and w.variant_of = $2 and ($3::uuid is null or w.id <> $3) order by w.id, o.idx",
    )
    .bind(tenant)
    .bind(parent)
    .bind(except)
    .fetch_all(&mut **tx)
    .await?;
    let mut by: Vec<(Uuid, Options)> = Vec::new();
    for (w, o, v) in found {
        match by.iter_mut().find(|(id, _)| *id == w) {
            Some((_, opts)) => opts.push((o, v)),
            None => by.push((w, vec![(o, v)])),
        }
    }
    Ok(by.into_iter().map(|(_, o)| o).collect())
}

#[async_trait]
impl DocHook for WebsiteItemHook {
    async fn before_validate(&self, hc: &mut HookCtx<'_>, doc: &mut Doc) -> Result<()> {
        // Attributes are trimmed (the rows keep their ids).
        if doc.get("attributes").is_some_and(Value::is_array) {
            let trimmed: Vec<Value> = rows(doc, "attributes")
                .into_iter()
                .map(|mut r| {
                    for f in ["attribute", "value"] {
                        if let Some(v) = s(&r, f).map(|v| v.trim().to_string()) {
                            set(&mut r, f, v);
                        }
                    }
                    Value::Object(r)
                })
                .collect();
            set(doc, "attributes", Value::Array(trimmed));
        }
        // Options belong to variants only, and are tidied: blanks dropped, spaces trimmed (the rows keep their ids).
        let parent = id(doc, "variant_of");
        let tidy: Vec<Value> = if parent.is_none() {
            Vec::new()
        } else {
            rows(doc, "options")
                .into_iter()
                .filter_map(|mut r| {
                    let (o, v) = (s(&r, "option")?.trim().to_string(), s(&r, "value")?.trim().to_string());
                    if o.is_empty() || v.is_empty() {
                        return None;
                    }
                    set(&mut r, "option", o);
                    set(&mut r, "value", v);
                    if id(&r, "id").is_none() {
                        set(&mut r, "id", Uuid::now_v7().to_string());
                    }
                    Some(Value::Object(r))
                })
                .collect()
        };
        set(doc, "options", Value::Array(tidy));
        let options = options_of(doc);
        if s(doc, "title").is_none() {
            if let Some(p) = parent.filter(|_| !options.is_empty()) {
                let parent = get(hc.kernel, hc.ctx, hc.tx, "WebsiteItem", p).await?;
                set(doc, "title", variant_title(s(&parent, "title").unwrap_or_default(), &options));
            } else if let Some(item) = id(doc, "item") {
                let it = get(hc.kernel, hc.ctx, hc.tx, "Item", item).await?;
                set(doc, "title", s(&it, "item_name").unwrap_or_default().to_string());
            }
        }
        let base = s(doc, "slug").or_else(|| s(doc, "title")).unwrap_or_default().to_string();
        set(doc, "slug", slugify(&base));
        let me = id(doc, "id");
        for (table, field) in [("related_items", "website_item"), ("categories", "category")] {
            let mut seen = HashSet::new();
            let kept: Vec<Value> =
                rows(doc, table).into_iter().filter(|r| id(r, field).is_some_and(|x| Some(x) != me && seen.insert(x))).map(Value::Object).collect();
            set(doc, table, Value::Array(kept));
        }
        Ok(())
    }

    async fn validate(&self, hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        let attrs = rows(doc, "attributes");
        if attrs.len() > 40 {
            return Err(Error::field("attributes", "at most 40 attributes"));
        }
        let mut seen = HashSet::new();
        for r in &attrs {
            let (a, v) = (s(r, "attribute").unwrap_or_default(), s(r, "value").unwrap_or_default());
            if !seen.insert((a.to_lowercase(), v.to_lowercase())) {
                return Err(Error::field("attributes", format!("{a}: {v} is listed twice")));
            }
        }
        if let Some(parent) = id(doc, "variant_of") {
            let me = id(doc, "id");
            if Some(parent) == me {
                return Err(Error::field("variant_of", "an item cannot be a variant of itself"));
            }
            let p = get(hc.kernel, hc.ctx, hc.tx, "WebsiteItem", parent).await?;
            if id(&p, "variant_of").is_some() {
                return Err(Error::field("variant_of", "that item is a variant itself: variants go one level deep"));
            }
            if let Some(me) = me {
                let mine = find(hc.kernel, hc.ctx, hc.tx, "WebsiteItem", vec![Filter::eq("variant_of", me.to_string())], None, 1).await?;
                if !mine.is_empty() {
                    return Err(Error::field("variant_of", "this item has variants of its own"));
                }
            }
            let siblings = sibling_options(hc.tx, hc.ctx.tenant, parent, me).await?;
            if let Some(first) = option_problems(&options_of(doc), &siblings).into_iter().next() {
                return Err(Error::field("options", first));
            }
        }
        let slug = s(doc, "slug").unwrap_or_default();
        if slug.is_empty() {
            return Err(Error::field("slug", "use some letters or digits"));
        }
        if slug_taken(hc.kernel, hc.ctx, hc.tx, "WebsiteItem", slug, id(doc, "id")).await? {
            return Err(Error::field("slug", "another website item already uses this slug"));
        }
        if let Some(item) = id(doc, "item") {
            let twins = find(hc.kernel, hc.ctx, hc.tx, "WebsiteItem", vec![Filter::eq("item", item.to_string())], None, 2).await?;
            if twins.iter().any(|w| id(w, "id") != id(doc, "id")) {
                return Err(Error::field("item", "this item already has a website item"));
            }
        }
        if b(doc, "published")
            && let Some(item) = id(doc, "item")
            && b(&get(hc.kernel, hc.ctx, hc.tx, "Item", item).await?, "disabled")
        {
            return Err(Error::field("published", "the item is disabled"));
        }
        // A value the store has disabled on the product is not sold: its combinations stay unpublished.
        if b(doc, "published")
            && let Some(parent) = id(doc, "variant_of")
        {
            let p = get(hc.kernel, hc.ctx, hc.tx, "WebsiteItem", parent).await?;
            if crate::variants::has_disabled(&options_of(doc), &crate::variants::disabled_of(&p)) {
                return Err(Error::field("published", "the store has disabled one of this variant's options: it is not sold"));
            }
        }
        Ok(())
    }

    async fn action(&self, hc: &mut HookCtx<'_>, name: &str, doc: &Doc, params: &Value) -> Result<Option<Value>> {
        Ok(Some(match name {
            "generate_variants" => crate::variants::generate(hc, doc, params).await?,
            "submit_review" => submit_review(hc, doc, params).await?,
            "stock_alert" => stock_alert(hc, doc, params).await?,
            _ => return Ok(None),
        }))
    }
}

pub struct ReviewHook;

/// Whether `email` (or `customer`) bought one of `items` on an order that was not cancelled.
pub async fn bought(tx: &mut Tx, tenant: Uuid, items: &[Uuid], email: Option<&str>, customer: Option<Uuid>) -> Result<bool> {
    Ok(sqlx::query_scalar(
        "select exists(select 1 from e_sales_order o join e_sales_order__items i on i.tenant_id = o.tenant_id and i.parent_id = o.id \
         left join e_customer c on c.tenant_id = o.tenant_id and c.id = o.customer \
         where o.tenant_id = $1 and o.docstatus = 1 and i.item = any($2) \
         and (o.customer = $3 or ($4::text is not null and (lower(c.email) = $4 or lower(o.ext->>'commerce_contact_email') = $4))))",
    )
    .bind(tenant)
    .bind(items)
    .bind(customer)
    .bind(email.map(str::to_lowercase))
    .fetch_one(&mut **tx)
    .await?)
}

async fn refresh_rating(hc: &mut HookCtx<'_>, website_item: Uuid, excluding: Option<Uuid>) -> Result<()> {
    let (avg, count): (Option<Decimal>, i64) = sqlx::query_as(
        "select avg(rating)::numeric, count(*) from e_item_review where tenant_id = $1 and website_item = $2 and status = 'Approved' \
         and ($3::uuid is null or id <> $3)",
    )
    .bind(hc.ctx.tenant)
    .bind(website_item)
    .bind(excluding)
    .fetch_one(&mut **hc.tx)
    .await?;
    let mut v = Doc::new();
    v.insert("rating_average".into(), ferrum_domain::util::money(avg.unwrap_or_default().round_dp(2)));
    v.insert("review_count".into(), json!(count));
    hc.kernel.set_values_tx(&sys(hc.ctx), hc.tx, "WebsiteItem", website_item, v).await
}

#[async_trait]
impl DocHook for ReviewHook {
    async fn before_validate(&self, hc: &mut HookCtx<'_>, doc: &mut Doc) -> Result<()> {
        if s(doc, "status") == Some("Approved") && s(doc, "published_on").is_none() {
            set(doc, "published_on", hc.ctx.today().to_string());
        }
        let verified = match id(doc, "website_item") {
            Some(wi) => {
                let item = id(&get(hc.kernel, hc.ctx, hc.tx, "WebsiteItem", wi).await?, "item");
                match item {
                    Some(item) => {
                        // A variant bought is the item bought.
                        let mut items = vec![item];
                        items.extend(
                            sqlx::query_scalar::<_, Uuid>("select item from e_website_item where tenant_id = $1 and variant_of = $2")
                                .bind(hc.ctx.tenant)
                                .bind(wi)
                                .fetch_all(&mut **hc.tx)
                                .await?,
                        );
                        bought(hc.tx, hc.ctx.tenant, &items, s(doc, "reviewer_email"), id(doc, "customer")).await?
                    }
                    None => false,
                }
            }
            None => false,
        };
        set(doc, "verified_purchase", verified);
        Ok(())
    }

    async fn after_save(&self, hc: &mut HookCtx<'_>, doc: &Doc, old: Option<&Doc>) -> Result<()> {
        if let Some(wi) = id(doc, "website_item") {
            refresh_rating(hc, wi, None).await?;
        }
        if let Some(previous) = old.and_then(|o| id(o, "website_item")).filter(|p| Some(*p) != id(doc, "website_item")) {
            refresh_rating(hc, previous, None).await?;
        }
        Ok(())
    }

    async fn before_delete(&self, hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        match id(doc, "website_item") {
            Some(wi) => refresh_rating(hc, wi, id(doc, "id")).await,
            None => Ok(()),
        }
    }
}

// ───────────────────────────── prices and stock ─────────────────────────────

#[derive(Debug, Clone)]
pub struct Prices {
    pub list: Decimal,
    pub price: Decimal,
    pub discount_percent: Decimal,
    pub with_tax: Option<Decimal>,
}

impl Prices {
    pub fn view(&self, currency: &str) -> Value {
        let m = |v: Decimal| v.normalize().to_string();
        json!({"currency": currency, "list_price": m(self.list), "price": m(self.price), "discount_percent": m(self.discount_percent),
               "price_with_tax": self.with_tax.map(m)})
    }
}

/// One unit's price including tax, by the same engine and tax rows as the cart (ERP's mapping of
/// them). Fixed (`Actual`) charges are left out: they belong to an order, not to a unit.
// The engine's inputs gain fields over time; those not named here take their defaults.
#[allow(clippy::needless_update)]
fn unit_with_tax(line: &Doc, taxes: &[Doc], precision: u32, rounding: Rounding) -> Option<Decimal> {
    let taxes_in: Vec<TaxIn> =
        pricing::tax_in(taxes).into_iter().map(|t| if t.charge_type == ChargeType::Actual { TaxIn { amount: Decimal::ZERO, ..t } } else { t }).collect();
    let input = CalcInput {
        precision,
        rounding,
        lines: vec![LineIn {
            key: "u".into(),
            qty: Decimal::ONE,
            price: d(line, "price_list_rate"),
            discount_percent: d(line, "discount_percent"),
            tax_rates: pricing::line_tax_rates(line, taxes),
            ..Default::default()
        }],
        taxes: taxes_in,
        ..Default::default()
    };
    ferrum_calc::compute(&input).ok().map(|o| o.grand_total)
}

/// Unit prices of items in the store currency for one unit each, as a cart would price them
/// (price list, pricing rules for the customer if known, tax rows), computed in one pass.
pub async fn price_items(k: &Kernel, ctx: &Ctx, tx: &mut Tx, store: &Store, items: &[Uuid], customer: Option<Uuid>) -> Result<HashMap<Uuid, Prices>> {
    let mut wanted: Vec<Uuid> = items.to_vec();
    wanted.sort();
    wanted.dedup();
    if wanted.is_empty() {
        return Ok(HashMap::new());
    }
    let ent = k.entity(ctx, "ShoppingCart")?;
    let mut doc = Doc::new();
    set(&mut doc, "company", store.company.to_string());
    set(&mut doc, "posting_date", ctx.today().to_string());
    set(&mut doc, "currency", store.currency.to_string());
    set(&mut doc, "price_list", store.price_list.to_string());
    if let Some(t) = store.tax_template {
        set(&mut doc, "tax_template", t.to_string());
    }
    if let Some(c) = customer {
        set(&mut doc, "customer", c.to_string());
    }
    let lines: Vec<Value> = wanted.iter().map(|i| json!({"id": Uuid::now_v7().to_string(), "item": i.to_string(), "qty": "1"})).collect();
    set(&mut doc, "items", Value::Array(lines));
    let co = calculate(k, &sys(ctx), tx, &ent, &mut doc, Side::Selling).await?;
    let taxes = rows(&doc, "taxes");
    Ok(rows(&doc, "items")
        .iter()
        .filter_map(|r| {
            let item = id(r, "item")?;
            let with_tax = if store.display_prices_with_tax { unit_with_tax(r, &taxes, store.precision, co.rounding) } else { None };
            Some((item, Prices { list: d(r, "price_list_rate"), price: d(r, "rate"), discount_percent: d(r, "discount_percent"), with_tax }))
        })
        .collect())
}

/// Stock items among `items` (the others never run out).
pub async fn stock_items(tx: &mut Tx, tenant: Uuid, items: &[Uuid]) -> Result<HashSet<Uuid>> {
    let ids: Vec<Uuid> = sqlx::query_scalar("select id from e_item where tenant_id = $1 and id = any($2) and is_stock_item")
        .bind(tenant)
        .bind(items)
        .fetch_all(&mut **tx)
        .await?;
    Ok(ids.into_iter().collect())
}

/// Sellable quantity per item: stock on hand in the store's warehouses minus what open sales
/// orders still have to deliver from them (their reservation).
pub async fn available(tx: &mut Tx, tenant: Uuid, store: &Store, items: &[Uuid]) -> Result<HashMap<Uuid, Decimal>> {
    let rows: Vec<(Uuid, Decimal)> = sqlx::query_as(
        "with onhand as (select item_id as item, sum(qty) as qty from stock_balance where tenant_id = $1 and item_id = any($2) and warehouse_id = any($3) \
           group by item_id), \
         reserved as (select si.item, sum(greatest(si.qty - coalesce(si.delivered_qty, 0), 0)) as qty from e_sales_order so \
           join e_sales_order__items si on si.tenant_id = so.tenant_id and si.parent_id = so.id \
           where so.tenant_id = $1 and so.docstatus = 1 and so.status in ('To Deliver and Bill', 'To Deliver') and si.item = any($2) \
           and si.warehouse = any($3) group by si.item) \
         select t.i, coalesce(o.qty, 0) - coalesce(r.qty, 0) from unnest($2::uuid[]) as t(i) left join onhand o on o.item = t.i left join reserved r on r.item = t.i",
    )
    .bind(tenant)
    .bind(items)
    .bind(&store.warehouses)
    .fetch_all(&mut **tx)
    .await?;
    Ok(rows.into_iter().collect())
}

/// Sellable quantity per item and warehouse, as [`available`] counts it (which is their sum).
pub async fn available_by_warehouse(tx: &mut Tx, tenant: Uuid, store: &Store, items: &[Uuid]) -> Result<HashMap<(Uuid, Uuid), Decimal>> {
    let rows: Vec<(Uuid, Uuid, Decimal)> = sqlx::query_as(
        "with onhand as (select item_id as item, warehouse_id as wh, sum(qty) as qty from stock_balance where tenant_id = $1 and item_id = any($2) \
           and warehouse_id = any($3) group by item_id, warehouse_id), \
         reserved as (select si.item, si.warehouse as wh, sum(greatest(si.qty - coalesce(si.delivered_qty, 0), 0)) as qty from e_sales_order so \
           join e_sales_order__items si on si.tenant_id = so.tenant_id and si.parent_id = so.id \
           where so.tenant_id = $1 and so.docstatus = 1 and so.status in ('To Deliver and Bill', 'To Deliver') and si.item = any($2) \
           and si.warehouse = any($3) group by si.item, si.warehouse) \
         select i, w, coalesce(o.qty, 0) - coalesce(r.qty, 0) from unnest($2::uuid[]) as t(i) cross join unnest($3::uuid[]) as h(w) \
         left join onhand o on o.item = t.i and o.wh = h.w left join reserved r on r.item = t.i and r.wh = h.w",
    )
    .bind(tenant)
    .bind(items)
    .bind(&store.warehouses)
    .fetch_all(&mut **tx)
    .await?;
    Ok(rows.into_iter().map(|(i, w, q)| ((i, w), q)).collect())
}

/// Where a line of `qty` is fulfilled from: the fulfilment warehouse when it holds enough, else the
/// first of the store's other warehouses (in Store Settings' order) that does, else the fulfilment
/// warehouse (whose shortage the delivery then reports). What it takes is taken off `left`, so the
/// lines of one order do not count the same units twice.
pub fn allocate(store: &Store, left: &mut HashMap<(Uuid, Uuid), Decimal>, item: Uuid, qty: Decimal) -> Uuid {
    let order = std::iter::once(store.fulfilment_warehouse).chain(store.warehouses.iter().copied().filter(|w| *w != store.fulfilment_warehouse));
    let chosen = order.clone().find(|w| left.get(&(item, *w)).copied().unwrap_or_default() >= qty).unwrap_or(store.fulfilment_warehouse);
    if let Some(have) = left.get_mut(&(item, chosen)) {
        *have -= qty;
    }
    chosen
}

/// A store item's back-order terms, and the day it ships from while that is still to come.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Later {
    pub backorder: bool,
    pub limit: Option<Decimal>,
    pub ships_from: Option<chrono::NaiveDate>,
}

/// The back-order terms and ships-from dates of the store items of `items` that have either (a date already past is
/// dropped). Empty for a workspace whose WebsiteItem has no `backorder` field yet.
pub async fn later(tx: &mut Tx, ctx: &Ctx, items: &[Uuid]) -> Result<HashMap<Uuid, Later>> {
    if ctx.schema.entity("WebsiteItem").is_none_or(|e| e.field("backorder").is_none()) || items.is_empty() {
        return Ok(HashMap::new());
    }
    let rows: Vec<(Uuid, bool, Option<Decimal>, Option<chrono::NaiveDate>)> = sqlx::query_as(
        "select item, coalesce(backorder, false), nullif(backorder_limit, 0), ships_from from e_website_item \
         where tenant_id = $1 and item = any($2) and (coalesce(backorder, false) or ships_from is not null)",
    )
    .bind(ctx.tenant)
    .bind(items)
    .fetch_all(&mut **tx)
    .await?;
    let today = ctx.today();
    Ok(rows.into_iter().map(|(item, backorder, limit, ships_from)| (item, Later { backorder, limit, ships_from: ships_from.filter(|d| *d > today) })).collect())
}

/// How many of a stock item may be ordered now, `available` being what is on hand less what open orders still owe
/// (negative once back-orders are owed): none for any quantity.
pub fn orderable(available: Decimal, terms: Option<&Later>) -> Option<Decimal> {
    match terms {
        Some(t) if t.backorder => t.limit.map(|limit| (available + limit).max(Decimal::ZERO)),
        _ => Some(available.max(Decimal::ZERO)),
    }
}

/// What the storefront says of one item's stock.
pub struct Stock {
    /// It can be ordered now: a non-stock item, one in stock, or one on back-order with room left.
    pub in_stock: bool,
    /// What is on hand, when the store shows quantities.
    pub qty: Option<String>,
    /// Out of stock and still ordered, to ship when it is back.
    pub backorder: bool,
    /// The day it ships from, while that is to come.
    pub ships_from: Option<String>,
}

/// [`Stock`] for one item, from its sellable quantity and its terms.
pub fn stock_view(store: &Store, is_stock: bool, available: Decimal, terms: Option<&Later>) -> Stock {
    let ships_from = terms.and_then(|t| t.ships_from).map(|d| d.to_string());
    if !is_stock {
        return Stock { in_stock: true, qty: None, backorder: false, ships_from };
    }
    let qty = available.max(Decimal::ZERO);
    let have = qty > Decimal::ZERO;
    let backorder = !have && terms.is_some_and(|t| t.backorder) && orderable(available, terms).is_none_or(|m| m > Decimal::ZERO);
    Stock { in_stock: have || backorder, qty: store.show_stock_quantity.then(|| qty.normalize().to_string()), backorder, ships_from }
}

// ───────────────────────────── listing ─────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sort {
    Ranking,
    PriceAsc,
    PriceDesc,
    Newest,
    Name,
    Rating,
}

impl Sort {
    pub fn parse(v: Option<&str>) -> Result<Sort> {
        Ok(match v.unwrap_or("ranking") {
            "ranking" => Sort::Ranking,
            "price_asc" => Sort::PriceAsc,
            "price_desc" => Sort::PriceDesc,
            "newest" => Sort::Newest,
            "name" => Sort::Name,
            "rating" => Sort::Rating,
            other => return Err(Error::field("sort", format!("unknown sort '{other}' (ranking, price_asc, price_desc, newest, name, rating)"))),
        })
    }

    pub(crate) fn name(self) -> &'static str {
        match self {
            Sort::Ranking => "ranking",
            Sort::PriceAsc => "price_asc",
            Sort::PriceDesc => "price_desc",
            Sort::Newest => "newest",
            Sort::Name => "name",
            Sort::Rating => "rating",
        }
    }

    /// (key column of `base`, SQL type of the key, descending)
    fn key(self) -> (&'static str, &'static str, bool) {
        match self {
            Sort::Ranking => ("ranking", "bigint", true),
            Sort::PriceAsc => ("price", "numeric", false),
            Sort::PriceDesc => ("price", "numeric", true),
            Sort::Newest => ("created_at", "timestamptz", true),
            Sort::Name => ("ltitle", "text", false),
            Sort::Rating => ("rating", "numeric", true),
        }
    }
}

#[derive(Debug, Default)]
pub struct Listing {
    pub category: Option<String>,
    pub q: Option<String>,
    pub min_price: Option<Decimal>,
    pub max_price: Option<Decimal>,
    pub in_stock: bool,
    pub ids: Option<Vec<Uuid>>,
    /// Chosen attribute values (lower case): an item must hold one of each attribute's values, as its own attribute
    /// or as an option of one of its published variants.
    pub attrs: Vec<(String, Vec<String>)>,
}

/// The `attr.<Attribute>=<Value>[,<Value>…]` filters of a listing's query, lower case and sorted by name, so the same
/// choice is always the same address: at most 8 attributes of at most 10 values, each name and value 1 to 60 characters.
pub fn attr_filters(query: &Map<String, Value>) -> Result<Vec<(String, Vec<String>)>> {
    let mut out: Vec<(String, Vec<String>)> = Vec::new();
    for (key, raw) in query {
        let Some(name) = key.strip_prefix("attr.") else { continue };
        let field = || format!("attr.{name}");
        let name = name.trim().to_lowercase();
        if name.is_empty() || name.chars().count() > 60 {
            return Err(Error::field(field(), "name an attribute of 1 to 60 characters"));
        }
        let text = raw.as_str().ok_or_else(|| Error::field(field(), "must be text"))?;
        let mut values: Vec<String> = text.split(',').map(|v| v.trim().to_lowercase()).filter(|v| !v.is_empty()).collect();
        values.sort();
        values.dedup();
        if values.is_empty() {
            continue;
        }
        if values.len() > 10 || values.iter().any(|v| v.chars().count() > 60) {
            return Err(Error::field(field(), "at most 10 values of at most 60 characters each"));
        }
        match out.iter_mut().find(|(n, _)| *n == name) {
            Some((_, vs)) => {
                vs.extend(values);
                vs.sort();
                vs.dedup();
            }
            None => out.push((name, values)),
        }
    }
    if out.len() > 8 {
        return Err(Error::field("attr", "filter by at most 8 attributes"));
    }
    out.sort();
    Ok(out)
}

#[derive(sqlx::FromRow)]
struct CardRow {
    id: Uuid,
    slug: Option<String>,
    title: Option<String>,
    short_description: Option<String>,
    is_stock: bool,
    available: Decimal,
    rating: Decimal,
    reviews: i64,
    /// The item whose price the card shows: its own, or a variant group's cheapest.
    price_item: Uuid,
    /// Something in the group can be bought now.
    in_stock: bool,
    /// Published variants the item has (it is then sold through them).
    variants: i64,
    /// A stock item out of stock, sold on back-order.
    backorder: bool,
    sort_key: Option<String>,
}

/// Published category ids under `slug` (itself included); `None` when there is no such category.
async fn category_ids(tx: &mut Tx, tenant: Uuid, slug: &str) -> Result<Option<Vec<Uuid>>> {
    let ids: Vec<Uuid> = sqlx::query_scalar(
        "with recursive tree as (select id from e_item_category where tenant_id = $1 and slug = $2 and published \
         union select c.id from e_item_category c join tree t on c.parent_category = t.id where c.tenant_id = $1 and c.published) select id from tree",
    )
    .bind(tenant)
    .bind(slug)
    .fetch_all(&mut **tx)
    .await?;
    Ok((!ids.is_empty()).then_some(ids))
}

/// Rate from the store currency to the company currency (list prices are company prices).
async fn to_company_rate(k: &Kernel, ctx: &Ctx, tx: &mut Tx, store: &Store) -> Result<Decimal> {
    let co = ferrum_domain::util::company(k, ctx, tx, store.company).await?;
    ferrum_domain::util::exchange_rate(k, ctx, tx, store.currency, co.currency_id, ctx.today()).await
}

/// The CTEs of a listing, `listed` last: the published store items with their price and stock, narrowed by the
/// category, the words, the price range and "in stock only" (everything but the attributes).
fn push_listed(qb: &mut QueryBuilder<Postgres>, ctx: &Ctx, store: &Store, f: &Listing, cats: Option<Vec<Uuid>>, rate: Decimal) {
    let tenant = ctx.tenant;
    let today = ctx.today();
    qb.push("with onhand as (select item_id as item, sum(qty) as qty from stock_balance where tenant_id = ");
    qb.push_bind(tenant).push(" and warehouse_id = any(").push_bind(store.warehouses.clone()).push(") group by item_id), ");
    qb.push("reserved as (select si.item, sum(greatest(si.qty - coalesce(si.delivered_qty, 0), 0)) as qty from e_sales_order so ");
    qb.push("join e_sales_order__items si on si.tenant_id = so.tenant_id and si.parent_id = so.id where so.tenant_id = ").push_bind(tenant);
    qb.push(" and so.docstatus = 1 and so.status in ('To Deliver and Bill', 'To Deliver') and si.warehouse = any(").push_bind(store.warehouses.clone());
    // Every published, enabled store item with its own price and stock...
    qb.push(") group by si.item), member as (select w.id, w.variant_of, w.item, coalesce(i.is_stock_item, false) as is_stock, ");
    qb.push("coalesce(o.qty, 0) - coalesce(r.qty, 0) as available, ");
    // A workspace not yet updated has no back-order field: nothing is on back-order there.
    qb.push(if ctx.schema.entity("WebsiteItem").is_some_and(|e| e.field("backorder").is_some()) { "coalesce(w.backorder, false)" } else { "false" });
    qb.push(" as backorder, ");
    qb.push("exists (select 1 from e_website_item v where v.tenant_id = w.tenant_id and v.variant_of = w.id and v.published) as has_variants, ");
    qb.push("coalesce((select p.rate from e_item_price p where p.tenant_id = w.tenant_id and p.item = w.item and p.price_list = ").push_bind(store.price_list);
    qb.push(" and p.valid_from <= ").push_bind(today).push(" and (p.valid_upto is null or p.valid_upto >= ").push_bind(today);
    qb.push(") order by p.valid_from desc limit 1), nullif(i.standard_rate, 0)) as price from e_website_item w ");
    qb.push("join e_item i on i.tenant_id = w.tenant_id and i.id = w.item left join onhand o on o.item = w.item left join reserved r on r.item = w.item ");
    qb.push("where w.tenant_id = ").push_bind(tenant).push(" and w.published and not coalesce(i.disabled, false)), ");
    // ...and, per listed item, what it is sold for and whether anything can be bought: its own, or
    // its variants' (an item with variants is sold through them, so its own price does not count).
    qb.push("grp as (select coalesce(m.variant_of, m.id) as gid, min(m.price) filter (where m.price > 0 and not m.has_variants) as price, ");
    qb.push("(array_agg(m.item order by m.price asc) filter (where m.price > 0 and not m.has_variants))[1] as price_item, ");
    // A limit used up is not seen here (the product page and the cart say it): the listing reads no limits.
    qb.push("coalesce(bool_or(not m.is_stock or m.available > 0 or m.backorder) filter (where not m.has_variants), false) as in_stock, ");
    qb.push("count(*) filter (where m.variant_of is not null) as variants from member m group by 1), ");
    qb.push("base as (select w.id, w.slug, w.title, w.short_description, coalesce(w.ranking, 0) as ranking, w.created_at, ");
    qb.push("lower(coalesce(w.title, '')) as ltitle, coalesce(w.rating_average, 0) as rating, coalesce(w.review_count, 0) as reviews, w.item, ");
    qb.push("m.is_stock, m.available, (m.backorder and m.is_stock and m.available <= 0) as backorder, g.price, coalesce(g.price_item, w.item) as price_item, g.in_stock, g.variants from e_website_item w ");
    qb.push("join member m on m.id = w.id join grp g on g.gid = w.id join e_item i on i.tenant_id = w.tenant_id and i.id = w.item ");
    qb.push("where w.tenant_id = ").push_bind(tenant).push(" and w.variant_of is null");
    if let Some(ids) = &f.ids {
        qb.push(" and w.id = any(").push_bind(ids.clone()).push(")");
    }
    if let Some(cats) = cats {
        qb.push(" and exists (select 1 from e_website_item__categories c where c.tenant_id = w.tenant_id and c.parent_id = w.id and c.category = any(");
        qb.push_bind(cats).push("))");
    }
    // Each word must be found somewhere: in the name, the short text or the code of the item or of
    // one of its variants, or in the choice that sets a variant apart ("red").
    for term in f.q.as_deref().map(search_terms).unwrap_or_default() {
        let pat = format!("%{}%", escape_like(&term));
        qb.push(" and (w.title ilike ").push_bind(pat.clone()).push(" or w.short_description ilike ").push_bind(pat.clone());
        qb.push(" or i.item_code ilike ").push_bind(pat.clone());
        qb.push(" or exists (select 1 from e_website_item v join e_item vi on vi.tenant_id = v.tenant_id and vi.id = v.item where v.tenant_id = w.tenant_id and v.variant_of = w.id and v.published and (v.title ilike ");
        qb.push_bind(pat.clone()).push(" or vi.item_code ilike ").push_bind(pat.clone());
        qb.push(" or exists (select 1 from e_website_item__options vo where vo.tenant_id = v.tenant_id and vo.parent_id = v.id and vo.\"value\" ilike ")
            .push_bind(pat)
            .push("))))");
    }
    // What the listing holds before any attribute is chosen: the cards and the facets read the same set.
    qb.push("), listed as (select * from base where price > 0");
    if let Some(min) = f.min_price {
        qb.push(" and price >= ").push_bind(min * rate);
    }
    if let Some(max) = f.max_price {
        qb.push(" and price <= ").push_bind(max * rate);
    }
    if f.in_stock {
        qb.push(" and in_stock");
    }
    qb.push(")");
}

/// One condition per chosen attribute on a listed row `l`: the item holds one of the values, as its own attribute or
/// as an option of a published variant (a workspace not yet updated has only the variants' options).
fn push_attrs(qb: &mut QueryBuilder<Postgres>, ctx: &Ctx, attrs: &[(String, Vec<String>)]) {
    let own = ctx.schema.entity("WebsiteItemAttribute").is_some();
    for (name, values) in attrs {
        qb.push(" and (");
        if own {
            qb.push("exists (select 1 from e_website_item__attributes a where a.tenant_id = ").push_bind(ctx.tenant);
            qb.push(" and a.parent_id = l.id and lower(a.\"attribute\") = ").push_bind(name.clone());
            qb.push(" and lower(a.\"value\") = any(").push_bind(values.clone()).push(")) or ");
        }
        qb.push(
            "exists (select 1 from e_website_item v join e_website_item__options o on o.tenant_id = v.tenant_id and o.parent_id = v.id where v.tenant_id = ",
        );
        qb.push_bind(ctx.tenant).push(" and v.variant_of = l.id and v.published and lower(o.\"option\") = ").push_bind(name.clone());
        qb.push(" and lower(o.\"value\") = any(").push_bind(values.clone()).push(")))");
    }
}

/// One page of website item cards. Returns the cards and the cursor of the next page.
#[allow(clippy::too_many_arguments)]
pub async fn cards(
    k: &Kernel,
    ctx: &Ctx,
    tx: &mut Tx,
    store: &Store,
    f: &Listing,
    sort: Sort,
    limit: i64,
    cursor: Option<&str>,
    customer: Option<Uuid>,
) -> Result<(Vec<Value>, Option<String>)> {
    let tenant = ctx.tenant;
    let cats = match &f.category {
        Some(slug) => Some(category_ids(tx, tenant, slug).await?.ok_or_else(|| Error::NotFound("category".into()))?),
        None => None,
    };
    let rate = if f.min_price.is_some() || f.max_price.is_some() { to_company_rate(k, ctx, tx, store).await? } else { Decimal::ONE };
    let (key, key_type, desc) = sort.key();
    let after = match cursor {
        Some(c) => match decode_cursor(c)?.as_slice() {
            [s, v, i] if s == sort.name() => Some((v.clone(), Uuid::parse_str(i).map_err(|_| Error::invalid("invalid cursor"))?)),
            _ => return Err(Error::invalid("invalid cursor (or one made for another sort)")),
        },
        None => None,
    };
    let mut qb: QueryBuilder<Postgres> = QueryBuilder::new("");
    push_listed(&mut qb, ctx, store, f, cats, rate);
    qb.push(format!(
        " select id, slug, title, short_description, is_stock, available, rating, reviews, price_item, in_stock, variants, backorder, ({key})::text as sort_key from listed l where true"
    ));
    push_attrs(&mut qb, ctx, &f.attrs);
    if let Some((v, after_id)) = &after {
        qb.push(format!(" and ({key}, id) {} (", if desc { "<" } else { ">" }))
            .push_bind(v.clone())
            .push(format!("::{key_type}, "))
            .push_bind(*after_id)
            .push(")");
    }
    let dir = if desc { "desc" } else { "asc" };
    qb.push(format!(" order by {key} {dir}, id {dir} limit ")).push_bind(limit + 1);
    let mut found: Vec<CardRow> = qb.build_query_as().fetch_all(&mut **tx).await?;
    let more = found.len() as i64 > limit;
    found.truncate(limit as usize);
    let next =
        if more { found.last().map(|r| encode_cursor(&[sort.name().to_string(), r.sort_key.clone().unwrap_or_default(), r.id.to_string()])) } else { None };
    let out = render(k, ctx, tx, store, &found, customer).await?;
    Ok((out, next))
}

/// What a listing can be narrowed by: each attribute of its items (their own, and their published variants' options)
/// with its values and, for each, how many cards the listing would show with that value added to the current choice
/// (an attribute's own choice left out, so the other values of a chosen attribute still count). The attributes with
/// the most items first, at most 12, each with at most 20 values, the most common first.
pub async fn facets(k: &Kernel, ctx: &Ctx, tx: &mut Tx, store: &Store, f: &Listing) -> Result<Vec<Value>> {
    let cats = match &f.category {
        Some(slug) => Some(category_ids(tx, ctx.tenant, slug).await?.ok_or_else(|| Error::NotFound("category".into()))?),
        None => None,
    };
    let rate = if f.min_price.is_some() || f.max_price.is_some() { to_company_rate(k, ctx, tx, store).await? } else { Decimal::ONE };
    let names: Vec<String> = f.attrs.iter().map(|(n, _)| n.clone()).collect();
    let (pair_names, pair_values): (Vec<String>, Vec<String>) = f.attrs.iter().flat_map(|(n, vs)| vs.iter().map(move |v| (n.clone(), v.clone()))).unzip();
    let mut qb: QueryBuilder<Postgres> = QueryBuilder::new("");
    push_listed(&mut qb, ctx, store, f, cats, rate);
    qb.push(", av as (");
    if ctx.schema.entity("WebsiteItemAttribute").is_some() {
        qb.push("select l.id as item, lower(a.\"attribute\") as lname, lower(a.\"value\") as lvalue, a.\"attribute\" as name, a.\"value\" as value ");
        qb.push("from listed l join e_website_item__attributes a on a.tenant_id = ").push_bind(ctx.tenant).push(" and a.parent_id = l.id union ");
    }
    qb.push("select l.id, lower(o.\"option\"), lower(o.\"value\"), o.\"option\", o.\"value\" from listed l join e_website_item v on v.tenant_id = ");
    qb.push_bind(ctx.tenant)
        .push(" and v.variant_of = l.id and v.published join e_website_item__options o on o.tenant_id = v.tenant_id and o.parent_id = v.id), ");
    qb.push("chosen as (select * from unnest(").push_bind(pair_names).push("::text[], ").push_bind(pair_values).push("::text[]) as c(name, value)), ");
    qb.push(
        "sat as (select av.item, array_agg(distinct av.lname) as names from av join chosen c on c.name = av.lname and c.value = av.lvalue group by av.item), ",
    );
    // An item counts for a value when it satisfies every chosen attribute but that value's own.
    qb.push("kept as (select av.* from av left join sat on sat.item = av.item where array_remove(")
        .push_bind(names)
        .push("::text[], av.lname) <@ coalesce(sat.names, '{}')), ");
    qb.push("per_attr as (select lname, count(distinct item) as items, min(name) as name from kept group by lname) ");
    qb.push("select k.lname, k.lvalue, p.name, min(k.value) as value, count(distinct k.item) as n, p.items, ");
    qb.push("exists (select 1 from chosen c where c.name = k.lname and c.value = k.lvalue) as selected ");
    qb.push("from kept k join per_attr p on p.lname = k.lname group by k.lname, k.lvalue, p.name, p.items");
    let rows: Vec<(String, String, String, String, i64, i64, bool)> = qb.build_query_as().fetch_all(&mut **tx).await?;
    let mut attrs: Vec<(String, String, i64, Vec<(String, i64, bool)>)> = Vec::new();
    for (lname, _, name, value, n, items, selected) in rows {
        match attrs.iter_mut().find(|a| a.0 == lname) {
            Some(a) => a.3.push((value, n, selected)),
            None => attrs.push((lname, name, items, vec![(value, n, selected)])),
        }
    }
    attrs.sort_by(|a, b| b.2.cmp(&a.2).then_with(|| a.1.cmp(&b.1)));
    attrs.truncate(12);
    Ok(attrs
        .into_iter()
        .map(|(_, name, _, mut values)| {
            values.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
            values.truncate(20);
            json!({"attribute": name, "values": values.into_iter().map(|(value, count, selected)| json!({"value": value, "count": count, "selected": selected})).collect::<Vec<_>>()})
        })
        .collect())
}

async fn first_images(tx: &mut Tx, tenant: Uuid, ids: &[Uuid]) -> Result<HashMap<Uuid, Value>> {
    let rows: Vec<(Uuid, String, Option<String>)> = sqlx::query_as(
        "select distinct on (parent_id) parent_id, image, alt_text from e_website_item__images where tenant_id = $1 and parent_id = any($2) order by parent_id, idx",
    )
    .bind(tenant)
    .bind(ids)
    .fetch_all(&mut **tx)
    .await?;
    Ok(rows.into_iter().map(|(p, url, alt)| (p, json!({"url": url, "alt": alt}))).collect())
}

async fn render(k: &Kernel, ctx: &Ctx, tx: &mut Tx, store: &Store, found: &[CardRow], customer: Option<Uuid>) -> Result<Vec<Value>> {
    let ids: Vec<Uuid> = found.iter().map(|r| r.id).collect();
    let images = first_images(tx, ctx.tenant, &ids).await?;
    let items: Vec<Uuid> = found.iter().map(|r| r.price_item).collect();
    let prices = price_items(k, ctx, tx, store, &items, customer).await?;
    Ok(found
        .iter()
        .map(|r| {
            // An item with variants is in stock when one of them is; it shows no quantity of its own.
            let (in_stock, qty) = if r.variants > 0 {
                (r.in_stock, None)
            } else {
                let s = stock_view(store, r.is_stock, r.available, None);
                (s.in_stock || r.backorder, s.qty)
            };
            json!({
                "slug": r.slug,
                "title": r.title,
                "short_description": r.short_description,
                "image": images.get(&r.id),
                "price": prices.get(&r.price_item).map(|p| p.view(&store.currency_code)),
                "from": r.variants > 0,
                "in_stock": in_stock,
                "backorder": r.backorder && r.variants == 0,
                "available_qty": qty,
                "rating": {"average": r.rating.normalize().to_string(), "count": r.reviews},
            })
        })
        .collect())
}

/// Published categories as a tree, ordered by sort order then name.
pub async fn category_tree(tx: &mut Tx, tenant: Uuid) -> Result<Value> {
    #[derive(sqlx::FromRow)]
    struct Cat {
        id: Uuid,
        parent_category: Option<Uuid>,
        category_name: String,
        slug: Option<String>,
        image: Option<String>,
        description: Option<String>,
        seo_title: Option<String>,
        seo_description: Option<String>,
    }
    let cats: Vec<Cat> = sqlx::query_as(
        "select id, parent_category, category_name, slug, image, description, seo_title, seo_description from e_item_category \
         where tenant_id = $1 and published order by coalesce(sort_order, 0), category_name limit 2000",
    )
    .bind(tenant)
    .fetch_all(&mut **tx)
    .await?;
    let known: HashSet<Uuid> = cats.iter().map(|c| c.id).collect();
    let mut children: HashMap<Option<Uuid>, Vec<&Cat>> = HashMap::new();
    for c in &cats {
        // Unpublishing a category hides its whole branch.
        if c.parent_category.is_some_and(|p| !known.contains(&p)) {
            continue;
        }
        children.entry(c.parent_category).or_default().push(c);
    }
    fn build(parent: Option<Uuid>, children: &HashMap<Option<Uuid>, Vec<&Cat>>, depth: usize) -> Vec<Value> {
        if depth > 20 {
            return vec![];
        }
        children
            .get(&parent)
            .map(|list| {
                list.iter()
                    .map(|c| {
                        json!({"slug": c.slug, "name": c.category_name, "image": c.image, "description": c.description,
                               "seo": {"title": c.seo_title, "description": c.seo_description},
                               "children": build(Some(c.id), children, depth + 1)})
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
    Ok(Value::Array(build(None, &children, 0)))
}

/// A published website item by slug (whole, with child rows); its item must be enabled.
pub async fn published_by_slug(k: &Kernel, ctx: &Ctx, tx: &mut Tx, slug: &str) -> Result<Doc> {
    let found = find(k, ctx, tx, "WebsiteItem", vec![Filter::eq("slug", slug), Filter::eq("published", true)], None, 1).await?;
    let wid = found.first().and_then(|r| id(r, "id")).ok_or_else(|| Error::NotFound("item".into()))?;
    let wi = get(k, ctx, tx, "WebsiteItem", wid).await?;
    let item = get(k, ctx, tx, "Item", id(&wi, "item").ok_or_else(|| Error::NotFound("item".into()))?).await?;
    if b(&item, "disabled") {
        return Err(Error::NotFound("item".into()));
    }
    Ok(wi)
}

/// Approved reviews, newest first.
pub async fn reviews(tx: &mut Tx, tenant: Uuid, website_item: Uuid, limit: i64, cursor: Option<&str>) -> Result<(Vec<Value>, Option<String>)> {
    let after = match cursor {
        Some(c) => match decode_cursor(c)?.as_slice() {
            [ts, i] => Some((ts.clone(), Uuid::parse_str(i).map_err(|_| Error::invalid("invalid cursor"))?)),
            _ => return Err(Error::invalid("invalid cursor")),
        },
        None => None,
    };
    let mut qb: QueryBuilder<Postgres> = QueryBuilder::new(
        "select id, rating, title, review, reviewer_name, verified_purchase, coalesce(published_on, created_at::date)::text, created_at::text \
         from e_item_review where status = 'Approved' and tenant_id = ",
    );
    qb.push_bind(tenant).push(" and website_item = ").push_bind(website_item);
    if let Some((ts, i)) = &after {
        qb.push(" and (created_at, id) < (").push_bind(ts.clone()).push("::timestamptz, ").push_bind(*i).push(")");
    }
    qb.push(" order by created_at desc, id desc limit ").push_bind(limit + 1);
    let mut found: Vec<(Uuid, i64, Option<String>, Option<String>, Option<String>, Option<bool>, String, String)> =
        qb.build_query_as().fetch_all(&mut **tx).await?;
    let more = found.len() as i64 > limit;
    found.truncate(limit as usize);
    let next = if more { found.last().map(|r| encode_cursor(&[r.7.clone(), r.0.to_string()])) } else { None };
    let out = found
        .into_iter()
        .map(|(_, rating, title, review, name, verified, on, _)| {
            json!({"rating": rating, "title": title, "review": review, "reviewer_name": name, "verified_purchase": verified.unwrap_or(false), "date": on})
        })
        .collect();
    Ok((out, next))
}

/// A variant as the product page needs it for choosing: its address, what sets it apart, whether
/// it can be bought now.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Variant {
    pub slug: String,
    pub options: Options,
    pub in_stock: bool,
}

impl Variant {
    pub(crate) fn from_json(v: &Value) -> Option<Variant> {
        Some(Variant {
            slug: v["slug"].as_str()?.to_string(),
            options: v["options"].as_array()?.iter().filter_map(|o| Some((o["option"].as_str()?.to_string(), o["value"].as_str()?.to_string()))).collect(),
            in_stock: v["in_stock"].as_bool().unwrap_or(false) && v["purchasable"].as_bool().unwrap_or(false),
        })
    }
}

/// One choice among an option's values: the variant it leads to, whether it is the one shown, and
/// whether that variant can be bought now.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Choice {
    pub value: String,
    pub slug: String,
    pub selected: bool,
    pub in_stock: bool,
}

/// The variant shown first: the first one that can be bought, else the first.
pub(crate) fn default_variant(variants: &[Variant]) -> Option<&Variant> {
    variants.iter().find(|v| v.in_stock).or(variants.first())
}

/// For each option (Size, Colour) its values as links to a variant. A choice leads to the variant
/// that has it and agrees with the shown one on every other option, else to any variant that has it
/// (one that can be bought first), so a shopper never lands on a page without their choice.
pub(crate) fn option_groups(variants: &[Variant], selected: &str) -> Vec<(String, Vec<Choice>)> {
    let current: Options = variants.iter().find(|v| v.slug == selected).map(|v| v.options.clone()).unwrap_or_default();
    let mut names: Vec<String> = Vec::new();
    for (n, _) in variants.iter().flat_map(|v| v.options.iter()) {
        if !names.contains(n) {
            names.push(n.clone());
        }
    }
    let has = |v: &Variant, n: &str, val: &str| v.options.iter().any(|(vn, vv)| vn == n && vv == val);
    names
        .into_iter()
        .map(|name| {
            let mut values: Vec<String> = Vec::new();
            for (n, v) in variants.iter().flat_map(|v| v.options.iter()) {
                if *n == name && !values.contains(v) {
                    values.push(v.clone());
                }
            }
            let choices = values
                .into_iter()
                .map(|value| {
                    let leads: Vec<&Variant> = variants.iter().filter(|v| has(v, &name, &value)).collect();
                    let agrees: Vec<&Variant> =
                        leads.iter().copied().filter(|v| current.iter().filter(|(n, _)| *n != name).all(|(n, val)| has(v, n, val))).collect();
                    let pool = if agrees.is_empty() { &leads } else { &agrees };
                    let to = pool.iter().find(|v| v.in_stock).or(pool.first()).copied();
                    Choice {
                        selected: current.iter().any(|(n, v)| *n == name && *v == value),
                        in_stock: to.is_some_and(|v| v.in_stock),
                        slug: to.map(|v| v.slug.clone()).unwrap_or_default(),
                        value,
                    }
                })
                .collect();
            (name, choices)
        })
        .collect()
}

/// The published variants of a store item, as the storefront offers them: what sets each apart, its
/// own price and stock, and its picture; the ones people like best (ranking) first.
pub async fn variants_of(k: &Kernel, ctx: &Ctx, tx: &mut Tx, store: &Store, parent: Uuid, customer: Option<Uuid>) -> Result<Vec<Value>> {
    let found: Vec<(Uuid, Option<String>, Option<String>, Uuid)> = sqlx::query_as(
        "select w.id, w.slug, w.title, w.item from e_website_item w join e_item i on i.tenant_id = w.tenant_id and i.id = w.item \
         where w.tenant_id = $1 and w.variant_of = $2 and w.published and not coalesce(i.disabled, false) order by coalesce(w.ranking, 0) desc, w.created_at, w.id",
    )
    .bind(ctx.tenant)
    .bind(parent)
    .fetch_all(&mut **tx)
    .await?;
    if found.is_empty() {
        return Ok(vec![]);
    }
    let ids: Vec<Uuid> = found.iter().map(|v| v.0).collect();
    let items: Vec<Uuid> = found.iter().map(|v| v.3).collect();
    let opts: Vec<(Uuid, String, String)> = sqlx::query_as(
        "select parent_id, \"option\", \"value\" from e_website_item__options where tenant_id = $1 and parent_id = any($2) order by parent_id, idx",
    )
    .bind(ctx.tenant)
    .bind(&ids)
    .fetch_all(&mut **tx)
    .await?;
    let prices = price_items(k, ctx, tx, store, &items, customer).await?;
    let stocked = stock_items(tx, ctx.tenant, &items).await?;
    let avail = available(tx, ctx.tenant, store, &items).await?;
    let terms = later(tx, ctx, &items).await?;
    let images = first_images(tx, ctx.tenant, &ids).await?;
    Ok(found
        .into_iter()
        .map(|(id, slug, title, item)| {
            let price = prices.get(&item).filter(|p| p.list > Decimal::ZERO);
            let stock = stock_view(store, stocked.contains(&item), avail.get(&item).copied().unwrap_or_default(), terms.get(&item));
            json!({
                "slug": slug,
                "title": title,
                "options": opts.iter().filter(|(p, _, _)| *p == id).map(|(_, o, v)| json!({"option": o, "value": v})).collect::<Vec<_>>(),
                "price": price.map(|p| p.view(&store.currency_code)),
                "purchasable": price.is_some(),
                "in_stock": stock.in_stock,
                "backorder": stock.backorder,
                "ships_from": stock.ships_from,
                "available_qty": stock.qty,
                "image": images.get(&id),
            })
        })
        .collect())
}

/// Everything a product page needs: content, price, stock, categories, related items, reviews.
pub async fn item_page(k: &Kernel, ctx: &Ctx, tx: &mut Tx, store: &Store, slug: &str, customer: Option<Uuid>) -> Result<Value> {
    let wi = published_by_slug(k, ctx, tx, slug).await?;
    let wid = id(&wi, "id").ok_or_else(|| Error::internal("website item without id"))?;
    let item_id = id(&wi, "item").ok_or_else(|| Error::internal("website item without item"))?;
    let item = get(k, ctx, tx, "Item", item_id).await?;
    let prices = price_items(k, ctx, tx, store, &[item_id], customer).await?;
    let price = prices.get(&item_id).filter(|p| p.list > Decimal::ZERO);
    let is_stock = b(&item, "is_stock_item");
    let avail = available(tx, ctx.tenant, store, &[item_id]).await?.get(&item_id).copied().unwrap_or_default();
    let terms = later(tx, ctx, &[item_id]).await?;
    let stock = stock_view(store, is_stock, avail, terms.get(&item_id));
    let uom = match id(&item, "stock_uom") {
        Some(u) => {
            let u = get(k, ctx, tx, "Uom", u).await?;
            json!({"name": s(&u, "uom_name"), "whole_number": b(&u, "whole_number")})
        }
        None => Value::Null,
    };
    let cat_ids: Vec<Uuid> = rows(&wi, "categories").iter().filter_map(|r| id(r, "category")).collect();
    let categories: Vec<(Option<String>, String)> =
        sqlx::query_as("select slug, category_name from e_item_category where tenant_id = $1 and id = any($2) and published order by category_name")
            .bind(ctx.tenant)
            .bind(&cat_ids)
            .fetch_all(&mut **tx)
            .await?;
    let related_ids: Vec<Uuid> = rows(&wi, "related_items").iter().filter_map(|r| id(r, "website_item")).collect();
    let related = if related_ids.is_empty() {
        vec![]
    } else {
        let f = Listing { ids: Some(related_ids), ..Default::default() };
        cards(k, ctx, tx, store, &f, Sort::Ranking, 12, None, customer).await?.0
    };
    let (recent, more) = reviews(tx, ctx.tenant, wid, 10, None).await?;
    // A store item with variants is sold through them; a variant says which item it is one of.
    let variants = variants_of(k, ctx, tx, store, wid, customer).await?;
    let group = match id(&wi, "variant_of") {
        Some(parent) => {
            let p = get(k, ctx, tx, "WebsiteItem", parent).await?;
            Some(json!({"slug": s(&p, "slug"), "title": s(&p, "title"), "published": b(&p, "published")}))
        }
        None => None,
    };
    let options: Vec<Value> = rows(&wi, "options").iter().map(|o| json!({"option": s(o, "option"), "value": s(o, "value")})).collect();
    let site_max = d(&wi, "max_qty");
    let max_qty = if site_max > Decimal::ZERO { site_max.min(store.max_qty_per_line) } else { store.max_qty_per_line };
    Ok(json!({
        "slug": s(&wi, "slug"),
        "title": s(&wi, "title"),
        "sku": s(&item, "item_code"),
        "short_description": s(&wi, "short_description"),
        "description": s(&wi, "long_description"),
        "images": rows(&wi, "images").iter().map(|r| json!({"url": s(r, "image"), "alt": s(r, "alt_text")})).collect::<Vec<_>>(),
        "categories": categories.into_iter().map(|(slug, name)| json!({"slug": slug, "name": name})).collect::<Vec<_>>(),
        "price": price.map(|p| p.view(&store.currency_code)),
        "purchasable": price.is_some() && variants.is_empty(),
        "variant_of": group,
        "options": options,
        "variants": variants,
        "in_stock": stock.in_stock,
        "backorder": stock.backorder,
        "ships_from": stock.ships_from,
        "available_qty": stock.qty,
        "unit": uom,
        "max_qty": max_qty.normalize().to_string(),
        "seo": {"title": s(&wi, "seo_title").or(s(&wi, "title")), "description": s(&wi, "seo_description").or(s(&wi, "short_description"))},
        "rating": {"average": d(&wi, "rating_average").normalize().to_string(), "count": wi.get("review_count").and_then(Value::as_i64).unwrap_or(0)},
        "reviews": recent,
        "reviews_next_cursor": more,
        "related": related,
    }))
}

fn text_field(body: &Map<String, Value>, key: &str, max: usize) -> Result<Option<String>> {
    match body.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(v)) => {
            let v = v.trim();
            if v.chars().count() > max {
                return Err(Error::field(key, format!("at most {max} characters")));
            }
            Ok((!v.is_empty()).then(|| v.to_string()))
        }
        Some(_) => Err(Error::field(key, "must be text")),
    }
}

/// Refuses an item the storefront does not show: unpublished, or its item disabled (as `published_by_slug`).
async fn shown(hc: &mut HookCtx<'_>, wi: &Doc) -> Result<(Uuid, Uuid)> {
    let missing = || Error::NotFound("item".into());
    let (wid, item) = (id(wi, "id").ok_or_else(missing)?, id(wi, "item").ok_or_else(missing)?);
    if !b(wi, "published") || b(&get(hc.kernel, &sys(hc.ctx), hc.tx, "Item", item).await?, "disabled") {
        return Err(missing());
    }
    Ok((wid, item))
}

/// "Leave a Review": held for moderation. The reviewer's email and name are the caller's to give; `customer` (which
/// can make it a verified purchase) only for a reviewer who proved the email, so the action needs `write`.
async fn submit_review(hc: &mut HookCtx<'_>, wi: &Doc, params: &Value) -> Result<Value> {
    let (wid, _) = shown(hc, wi).await?;
    let empty = Map::new();
    let body = params.as_object().unwrap_or(&empty);
    let rating = body.get("rating").and_then(|v| v.as_i64().or_else(|| v.as_str().and_then(|s| s.trim().parse().ok())));
    let Some(rating) = rating.filter(|r| (1..=5).contains(r)) else {
        return Err(Error::field("rating", "rate the item from 1 to 5"));
    };
    let title = text_field(body, "title", 200)?;
    let review = text_field(body, "review", 5000)?;
    let email = text_field(body, "email", 254)?.ok_or_else(|| Error::field("email", "is required"))?.to_lowercase();
    let name = text_field(body, "name", 100)?.ok_or_else(|| Error::field("name", "is required"))?;
    let customer = match text_field(body, "customer", 36)? {
        Some(c) => Some(Uuid::parse_str(&c).map_err(|_| Error::field("customer", "must be a customer's id"))?),
        None => None,
    };
    let sctx = sys(hc.ctx);
    let dup =
        find(hc.kernel, &sctx, hc.tx, "ItemReview", vec![Filter::eq("website_item", wid.to_string()), Filter::eq("reviewer_email", email.clone())], None, 1)
            .await?;
    if !dup.is_empty() {
        return Err(Error::Conflict("you have already reviewed this item".into()));
    }
    let mut doc = Doc::new();
    set(&mut doc, "website_item", wid.to_string());
    set(&mut doc, "rating", rating);
    set(&mut doc, "title", json!(title));
    set(&mut doc, "review", json!(review));
    set(&mut doc, "reviewer_name", name);
    set(&mut doc, "reviewer_email", email);
    set(&mut doc, "customer", json!(customer.map(|c| c.to_string())));
    set(&mut doc, "status", "Pending");
    let made = hc.kernel.insert_tx(&sctx, hc.tx, "ItemReview", doc).await.map_err(|e| match e {
        Error::Validation(mut fields) => {
            for f in fields.iter_mut() {
                if f.field == "reviewer_email" {
                    f.field = "email".into();
                }
            }
            Error::Validation(fields)
        }
        other => other,
    })?;
    Ok(json!({"review": s(&made, "id"), "status": "Pending"}))
}

/// Parses an optional non-negative decimal query parameter.
pub fn decimal_param(raw: Option<&str>, name: &str) -> Result<Option<Decimal>> {
    match raw.map(str::trim).filter(|v| !v.is_empty()) {
        None => Ok(None),
        Some(v) => match Decimal::from_str(v) {
            Ok(x) if x >= Decimal::ZERO => Ok(Some(x)),
            _ => Err(Error::Validation(vec![FieldError::new(name, "must be a non-negative number")])),
        },
    }
}

/// "Tell Me When It Is Back": a shopper asks to be told when an out of stock item is back, once, by e-mail. The same
/// address asking again for the same item changes nothing; an item that can be bought now is not asked about; ten
/// waiting alerts an address is the most.
async fn stock_alert(hc: &mut HookCtx<'_>, wi: &Doc, params: &Value) -> Result<Value> {
    let email = crate::payment::param_text(params, "email", 1000)?.ok_or_else(|| Error::field("email", "is required"))?.to_lowercase();
    if email.len() > 254 || email.split('@').count() != 2 || email.starts_with('@') || email.ends_with('@') || email.chars().any(char::is_whitespace) {
        return Err(Error::field("email", "is not an e-mail address"));
    }
    if hc.ctx.schema.entity("StockAlert").is_none() {
        return Err(Error::NotFound("stock alerts".into()));
    }
    let (wid, item) = shown(hc, wi).await?;
    let sctx = sys(hc.ctx);
    let store = Store::load(hc.kernel, &sctx, hc.tx).await?;
    let stocked = stock_items(hc.tx, hc.ctx.tenant, &[item]).await?;
    let have = available(hc.tx, hc.ctx.tenant, &store, &[item]).await?.get(&item).copied().unwrap_or_default();
    let terms = later(hc.tx, hc.ctx, &[item]).await?;
    // One that can be ordered now, on back-order too, is not asked about.
    if !stocked.contains(&item) || !store.enforce_stock || orderable(have, terms.get(&item)).is_none_or(|m| m > Decimal::ZERO) {
        return Err(Error::invalid("this item is in stock: you can order it now"));
    }
    let mine = find(hc.kernel, &sctx, hc.tx, "StockAlert", vec![Filter::eq("email", email.clone()), Filter::eq("status", "Waiting")], None, 20).await?;
    if let Some(waiting) = mine.iter().find(|a| id(a, "website_item") == Some(wid)) {
        return Ok(json!({"status": "Waiting", "alert": s(waiting, "id")}));
    }
    if mine.len() >= 10 {
        return Err(Error::invalid("you are waiting for ten items already: we will write when one is back"));
    }
    let made = json!({"website_item": wid.to_string(), "item_title": s(wi, "title").or(s(wi, "item_name")), "item_slug": s(wi, "slug"), "email": email, "status": "Waiting"});
    let made = hc.kernel.insert_tx(&sctx, hc.tx, "StockAlert", made.as_object().cloned().unwrap_or_default()).await?;
    Ok(json!({"status": "Waiting", "alert": s(&made, "id")}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attr_filters_reads_names_values_and_limits() {
        let q = |v: Value| attr_filters(v.as_object().unwrap());
        assert_eq!(
            q(json!({"attr.Material": " Steel, wood ,", "attr.Brand": "ACME", "sort": "price_asc"})).unwrap(),
            vec![("brand".to_string(), vec!["acme".to_string()]), ("material".to_string(), vec!["steel".to_string(), "wood".to_string()])]
        );
        assert!(q(json!({"attr.Material": ""})).unwrap().is_empty(), "nothing chosen");
        let nine: Map<String, Value> = (0..9).map(|i| (format!("attr.a{i}"), json!("x"))).collect();
        assert!(attr_filters(&nine).is_err(), "at most 8 attributes");
        assert!(q(json!({"attr.Size": (0..11).map(|i| i.to_string()).collect::<Vec<_>>().join(",")})).is_err(), "at most 10 values");
        assert!(q(json!({"attr.Size": "x".repeat(61)})).is_err(), "at most 60 characters");
        assert!(q(json!({format!("attr.{}", "n".repeat(61)): "x"})).is_err());
    }

    #[test]
    fn orderable_counts_what_is_already_owed() {
        let n = |v: &str| v.parse::<Decimal>().unwrap();
        let back = |limit: Option<&str>| Later { backorder: true, limit: limit.map(n), ships_from: None };
        // No back-order: what is there, never below nothing.
        assert_eq!(orderable(n("3"), None), Some(n("3")));
        assert_eq!(orderable(n("-2"), Some(&Later::default())), Some(Decimal::ZERO));
        // Back-order without a limit: any quantity.
        assert_eq!(orderable(n("-40"), Some(&back(None))), None);
        // With a limit: what is there and the limit, less what earlier back-orders already owe.
        assert_eq!(orderable(n("1"), Some(&back(Some("2")))), Some(n("3")));
        assert_eq!(orderable(n("-1"), Some(&back(Some("2")))), Some(n("1")));
        assert_eq!(orderable(n("-5"), Some(&back(Some("2")))), Some(Decimal::ZERO));
    }

    #[test]
    fn slugs() {
        assert_eq!(slugify("  Blue Mug (Large)! "), "blue-mug-large");
        assert_eq!(slugify("Café au lait"), "café-au-lait");
        assert_eq!(slugify("--a__b--"), "a-b");
        assert_eq!(slugify("!!!"), "");
        assert_eq!(slugify(&"x".repeat(200)).len(), 140);
    }

    #[test]
    fn cursors_round_trip_and_reject_garbage() {
        let c = encode_cursor(&["price_asc".into(), "12.5".into(), Uuid::nil().to_string()]);
        assert_eq!(decode_cursor(&c).unwrap(), vec!["price_asc".to_string(), "12.5".into(), Uuid::nil().to_string()]);
        assert!(decode_cursor("not-a-cursor").is_err());
        assert!(decode_cursor(&URL_SAFE_NO_PAD.encode("{\"a\":1}")).is_err());
    }

    fn opts(pairs: &[(&str, &str)]) -> Options {
        pairs.iter().map(|(n, v)| (n.to_string(), v.to_string())).collect()
    }

    #[test]
    fn a_variant_says_what_sets_it_apart_and_is_not_a_twin() {
        assert_eq!(variant_title("Logo Tee", &opts(&[("Colour", "Red"), ("Size", "M")])), "Logo Tee — Red / M");
        let sibs = vec![opts(&[("Colour", "Red"), ("Size", "M")]), opts(&[("Colour", "Red"), ("Size", "L")])];
        assert!(option_problems(&opts(&[("colour", "blue"), ("Size", " M ")]), &sibs).is_empty(), "a new choice of the same options");
        assert!(option_problems(&opts(&[("Colour", "RED"), ("size", "m")]), &sibs)[0].contains("already"), "the same choices, however written");
        assert!(option_problems(&opts(&[("Colour", "Blue")]), &sibs)[0].contains("same options"), "a sibling names Size too");
        assert!(option_problems(&opts(&[("Size", "S"), ("size", "M")]), &[])[0].contains("twice"));
        assert!(option_problems(&opts(&[]), &[])[0].contains("what makes"));
        assert!(option_problems(&opts(&[("Size", "S")]), &[]).is_empty(), "the first variant of a group has no one to differ from");
    }

    fn variant(slug: &str, pairs: &[(&str, &str)], in_stock: bool) -> Variant {
        Variant { slug: slug.into(), options: opts(pairs), in_stock }
    }

    #[test]
    fn the_choices_of_an_item_with_variants() {
        let vs = vec![
            variant("tee-red-m", &[("Colour", "Red"), ("Size", "M")], true),
            variant("tee-red-l", &[("Colour", "Red"), ("Size", "L")], false),
            variant("tee-blue-m", &[("Colour", "Blue"), ("Size", "M")], true),
        ];
        assert_eq!(default_variant(&vs).unwrap().slug, "tee-red-m");
        assert_eq!(default_variant(&[variant("a", &[("Size", "S")], false)]).unwrap().slug, "a", "all out: still shown");
        assert!(default_variant(&[]).is_none());
        let groups = option_groups(&vs, "tee-red-m");
        assert_eq!(groups.iter().map(|(n, c)| (n.as_str(), c.len())).collect::<Vec<_>>(), vec![("Colour", 2), ("Size", 2)]);
        let size: Vec<(&str, &str, bool, bool)> = groups[1].1.iter().map(|c| (c.value.as_str(), c.slug.as_str(), c.selected, c.in_stock)).collect();
        assert_eq!(size, vec![("M", "tee-red-m", true, true), ("L", "tee-red-l", false, false)], "L keeps the colour, and is sold out");
        let colour: Vec<(&str, &str, bool)> = groups[0].1.iter().map(|c| (c.value.as_str(), c.slug.as_str(), c.selected)).collect();
        assert_eq!(colour, vec![("Red", "tee-red-m", true), ("Blue", "tee-blue-m", false)]);
        // From the sold-out red L, blue leads to the nearest blue that exists: M.
        let from_l = option_groups(&vs, "tee-red-l");
        assert_eq!(from_l[0].1[1].slug, "tee-blue-m");
        assert!(from_l[1].1[1].selected && !from_l[1].1[0].selected);
        assert!(option_groups(&[], "x").is_empty());
    }

    #[test]
    fn a_search_is_its_words_not_its_whole() {
        assert_eq!(search_terms("  mug   blue "), vec!["mug", "blue"]);
        assert_eq!(search_terms("a b c d e f g h").len(), 6);
        assert!(search_terms("   ").is_empty());
    }

    #[test]
    fn like_patterns_are_escaped() {
        assert_eq!(escape_like("50%_off\\"), "50\\%\\_off\\\\");
    }

    #[test]
    fn unit_price_with_tax_uses_item_overrides() {
        let taxes = vec![
            json!({"charge_type": "On Net Total", "account": "acc-gst", "rate": "18"}).as_object().cloned().unwrap(),
            json!({"charge_type": "Actual", "account": "acc-fee", "amount": "50"}).as_object().cloned().unwrap(),
        ];
        let line = |overrides: Value| json!({"price_list_rate": "100", "discount_percent": "10", "item_tax_rates": overrides}).as_object().cloned().unwrap();
        assert_eq!(unit_with_tax(&line(json!({})), &taxes, 2, Rounding::HalfUp), Some(Decimal::from_str("106.2").unwrap()), "fixed charges left out");
        assert_eq!(unit_with_tax(&line(json!({"acc-gst": "5"})), &taxes, 2, Rounding::HalfUp), Some(Decimal::from_str("94.5").unwrap()));
        let inclusive = vec![json!({"charge_type": "On Net Total", "account": "a", "rate": "18", "included_in_price": true}).as_object().cloned().unwrap()];
        assert_eq!(unit_with_tax(&line(json!({})), &inclusive, 2, Rounding::HalfUp), Some(Decimal::from_str("90").unwrap()), "already gross");
    }
}
