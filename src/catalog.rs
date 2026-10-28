//! The catalog: item categories (a tree), website items (the published face of an item) and
//! their moderated reviews, plus the public listing — prices from the pricing engine, stock
//! availability net of open orders, search, filters and keyset pagination.

use crate::store::Store;
use async_trait::async_trait;
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ferrum_calc::{CalcInput, ChargeType, LineIn, Rounding, TaxIn};
use ferrum_domain::pricing::{Side, calculate};
use ferrum_domain::util::{b, d, find, get, id, rows, s, set, sys};
use ferrum_kernel::{Ctx, Doc, DocHook, Error, FieldError, Filter, HookCtx, Kernel, Result, Tx};
use rust_decimal::Decimal;
use serde_json::{Map, Value, json};
use sqlx::{Postgres, QueryBuilder};
use std::collections::{BTreeMap, HashMap, HashSet};
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

#[async_trait]
impl DocHook for WebsiteItemHook {
    async fn before_validate(&self, hc: &mut HookCtx<'_>, doc: &mut Doc) -> Result<()> {
        if s(doc, "title").is_none()
            && let Some(item) = id(doc, "item")
        {
            let it = get(hc.kernel, hc.ctx, hc.tx, "Item", item).await?;
            set(doc, "title", s(&it, "item_name").unwrap_or_default().to_string());
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
        Ok(())
    }
}

pub struct ReviewHook;

/// Whether `email` (or `customer`) bought `item` on an order that was not cancelled.
pub async fn bought(tx: &mut Tx, tenant: Uuid, item: Uuid, email: Option<&str>, customer: Option<Uuid>) -> Result<bool> {
    Ok(sqlx::query_scalar(
        "select exists(select 1 from e_sales_order o join e_sales_order__items i on i.tenant_id = o.tenant_id and i.parent_id = o.id \
         left join e_customer c on c.tenant_id = o.tenant_id and c.id = o.customer \
         where o.tenant_id = $1 and o.docstatus = 1 and i.item = $2 \
         and (o.customer = $3 or ($4::text is not null and (lower(c.email) = $4 or lower(o.ext->>'commerce_contact_email') = $4))))",
    )
    .bind(tenant)
    .bind(item)
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
            set(doc, "published_on", crate::today().to_string());
        }
        let verified = match id(doc, "website_item") {
            Some(wi) => {
                let item = id(&get(hc.kernel, hc.ctx, hc.tx, "WebsiteItem", wi).await?, "item");
                match item {
                    Some(item) => bought(hc.tx, hc.ctx.tenant, item, s(doc, "reviewer_email"), id(doc, "customer")).await?,
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

fn charge_type(s: Option<&str>) -> ChargeType {
    match s {
        Some("On Previous Row Amount") => ChargeType::OnPreviousRowAmount,
        Some("On Previous Row Total") => ChargeType::OnPreviousRowTotal,
        Some("Actual") => ChargeType::Actual,
        _ => ChargeType::OnNetTotal,
    }
}

/// One unit's price including tax, by the same engine and tax rows as the cart. Fixed
/// (`Actual`) charges are left out: they belong to an order, not to a unit.
fn unit_with_tax(line: &Doc, taxes: &[Doc], precision: u32, rounding: Rounding) -> Option<Decimal> {
    let overrides = line.get("item_tax_rates").and_then(Value::as_object);
    let tax_in: Vec<TaxIn> = taxes
        .iter()
        .enumerate()
        .map(|(i, t)| TaxIn {
            key: format!("t{i}"),
            charge_type: charge_type(s(t, "charge_type")),
            rate: d(t, "rate"),
            amount: Decimal::ZERO,
            row_ref: t.get("row_ref").and_then(Value::as_i64).filter(|r| *r >= 1).map(|r| (r - 1) as usize),
            included_in_price: b(t, "included_in_price"),
            deduct: b(t, "deduct"),
        })
        .collect();
    let rates: BTreeMap<String, Decimal> = taxes
        .iter()
        .enumerate()
        .filter_map(|(i, t)| {
            let rate = overrides?.get(s(t, "account")?)?.as_str()?;
            Some((format!("t{i}"), Decimal::from_str(rate).ok()?))
        })
        .collect();
    let input = CalcInput {
        precision,
        base_precision: None,
        conversion_rate: Decimal::ONE,
        rounding,
        lines: vec![LineIn {
            key: "u".into(),
            qty: Decimal::ONE,
            price: d(line, "price_list_rate"),
            discount_percent: d(line, "discount_percent"),
            tax_rates: rates,
        }],
        taxes: tax_in,
        discount: None,
        round_total_to: None,
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
    set(&mut doc, "posting_date", crate::today().to_string());
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

/// `in_stock` (and the quantity when the store shows it) for one item.
pub fn stock_view(store: &Store, is_stock: bool, available: Decimal) -> (bool, Option<String>) {
    if !is_stock {
        return (true, None);
    }
    let qty = available.max(Decimal::ZERO);
    (qty > Decimal::ZERO, store.show_stock_quantity.then(|| qty.normalize().to_string()))
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

    fn name(self) -> &'static str {
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
}

#[derive(sqlx::FromRow)]
struct CardRow {
    id: Uuid,
    slug: Option<String>,
    title: Option<String>,
    short_description: Option<String>,
    item: Uuid,
    is_stock: bool,
    available: Decimal,
    rating: Decimal,
    reviews: i64,
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
    ferrum_domain::util::exchange_rate(k, ctx, tx, store.currency, co.currency_id, crate::today()).await
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
    let today = crate::today();
    let mut qb: QueryBuilder<Postgres> = QueryBuilder::new("with onhand as (select item_id as item, sum(qty) as qty from stock_balance where tenant_id = ");
    qb.push_bind(tenant).push(" and warehouse_id = any(").push_bind(store.warehouses.clone()).push(") group by item_id), ");
    qb.push("reserved as (select si.item, sum(greatest(si.qty - coalesce(si.delivered_qty, 0), 0)) as qty from e_sales_order so ");
    qb.push("join e_sales_order__items si on si.tenant_id = so.tenant_id and si.parent_id = so.id where so.tenant_id = ").push_bind(tenant);
    qb.push(" and so.docstatus = 1 and so.status in ('To Deliver and Bill', 'To Deliver') and si.warehouse = any(").push_bind(store.warehouses.clone());
    qb.push(") group by si.item), base as (select w.id, w.slug, w.title, w.short_description, coalesce(w.ranking, 0) as ranking, w.created_at, ");
    qb.push("lower(coalesce(w.title, '')) as ltitle, coalesce(w.rating_average, 0) as rating, coalesce(w.review_count, 0) as reviews, w.item, ");
    qb.push("coalesce(i.is_stock_item, false) as is_stock, coalesce(o.qty, 0) - coalesce(r.qty, 0) as available, coalesce((select p.rate from e_item_price p ");
    qb.push("where p.tenant_id = w.tenant_id and p.item = w.item and p.price_list = ").push_bind(store.price_list);
    qb.push(" and p.valid_from <= ").push_bind(today).push(" and (p.valid_upto is null or p.valid_upto >= ").push_bind(today);
    qb.push(") order by p.valid_from desc limit 1), nullif(i.standard_rate, 0)) as price from e_website_item w ");
    qb.push("join e_item i on i.tenant_id = w.tenant_id and i.id = w.item left join onhand o on o.item = w.item left join reserved r on r.item = w.item ");
    qb.push("where w.tenant_id = ").push_bind(tenant).push(" and w.published and not coalesce(i.disabled, false)");
    if let Some(ids) = &f.ids {
        qb.push(" and w.id = any(").push_bind(ids.clone()).push(")");
    }
    if let Some(cats) = cats {
        qb.push(" and exists (select 1 from e_website_item__categories c where c.tenant_id = w.tenant_id and c.parent_id = w.id and c.category = any(");
        qb.push_bind(cats).push("))");
    }
    if let Some(q) = f.q.as_deref().map(str::trim).filter(|q| !q.is_empty()) {
        let pat = format!("%{}%", escape_like(q));
        qb.push(" and (w.title ilike ").push_bind(pat.clone()).push(" or w.short_description ilike ").push_bind(pat.clone());
        qb.push(" or i.item_code ilike ").push_bind(pat).push(")");
    }
    qb.push(format!(
        ") select id, slug, title, short_description, item, is_stock, available, rating, reviews, ({key})::text as sort_key from base where price > 0"
    ));
    if let Some(min) = f.min_price {
        qb.push(" and price >= ").push_bind(min * rate);
    }
    if let Some(max) = f.max_price {
        qb.push(" and price <= ").push_bind(max * rate);
    }
    if f.in_stock {
        qb.push(" and (not is_stock or available > 0)");
    }
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
    let items: Vec<Uuid> = found.iter().map(|r| r.item).collect();
    let prices = price_items(k, ctx, tx, store, &items, customer).await?;
    Ok(found
        .iter()
        .map(|r| {
            let (in_stock, qty) = stock_view(store, r.is_stock, r.available);
            json!({
                "slug": r.slug,
                "title": r.title,
                "short_description": r.short_description,
                "image": images.get(&r.id),
                "price": prices.get(&r.item).map(|p| p.view(&store.currency_code)),
                "in_stock": in_stock,
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
    let (in_stock, qty) = stock_view(store, is_stock, avail);
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
        "purchasable": price.is_some(),
        "in_stock": in_stock,
        "available_qty": qty,
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

/// A review from the storefront, held for moderation. Signed-in customers review as themselves.
pub async fn submit_review(k: &Kernel, ctx: &Ctx, tx: &mut Tx, slug: &str, body: &Map<String, Value>, user: Option<&Ctx>) -> Result<Value> {
    let wi = published_by_slug(k, ctx, tx, slug).await?;
    let wid = id(&wi, "id").ok_or_else(|| Error::internal("website item without id"))?;
    let rating = body.get("rating").and_then(|v| v.as_i64().or_else(|| v.as_str().and_then(|s| s.trim().parse().ok())));
    let Some(rating) = rating.filter(|r| (1..=5).contains(r)) else {
        return Err(Error::field("rating", "rate the item from 1 to 5"));
    };
    let title = text_field(body, "title", 200)?;
    let review = text_field(body, "review", 5000)?;
    let (email, name, customer) = match user {
        Some(u) => {
            let customer = crate::orders::linked_customers(k, ctx, tx, &u.email).await?.into_iter().next();
            let name = text_field(body, "name", 100)?.unwrap_or_else(|| u.full_name.clone());
            (u.email.to_lowercase(), name, customer)
        }
        None => {
            let email = text_field(body, "email", 254)?.ok_or_else(|| Error::field("email", "is required"))?.to_lowercase();
            let name = text_field(body, "name", 100)?.ok_or_else(|| Error::field("name", "is required"))?;
            (email, name, None)
        }
    };
    let dup = find(k, ctx, tx, "ItemReview", vec![Filter::eq("website_item", wid.to_string()), Filter::eq("reviewer_email", email.clone())], None, 1).await?;
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
    k.insert_tx(&sys(ctx), tx, "ItemReview", doc).await.map_err(|e| match e {
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
    Ok(json!({"status": "Pending", "message": "Thank you! Your review appears once it has been approved."}))
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

#[cfg(test)]
mod tests {
    use super::*;

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
