//! Store reports: web sales by day, item or category, top products, abandoned carts, the
//! conversion funnel and coupon usage. Web orders are the orders checked out from carts;
//! revenue is in the company currency; each report sees only what the caller may read.

use async_trait::async_trait;
use ferrum_domain::reports::{Params, ReportCompany, ReportSource, col, param_def, period, read_tx, resolve_company};
use ferrum_domain::util::money;
use ferrum_kernel::{Action, Ctx, Error, Kernel, Result, Tx, row_scope};
use rust_decimal::Decimal;
use serde_json::{Value, json};
use sqlx::{Postgres, QueryBuilder};
use std::collections::HashMap;

pub struct CommerceReports;

fn select(name: &str, label: &str, options: &[&str]) -> Value {
    json!({"name": name, "label": label, "type": "select", "options": options, "default": options[0]})
}

#[async_trait]
impl ReportSource for CommerceReports {
    fn catalog(&self) -> Vec<Value> {
        let dates = || vec![param_def("company"), param_def("from_date"), param_def("to_date")];
        let mut web_sales = dates();
        web_sales.push(select("group_by", "Group By", &["Day", "Item", "Category"]));
        vec![
            json!({"name": "web_sales", "label": "Web Sales", "module": "commerce", "entity": "SalesOrder", "params": web_sales}),
            json!({"name": "top_products", "label": "Top Web Products", "module": "commerce", "entity": "SalesOrder", "params": dates()}),
            json!({"name": "abandoned_carts", "label": "Abandoned Carts", "module": "commerce", "entity": "ShoppingCart", "params": dates()}),
            json!({"name": "conversion_funnel", "label": "Web Conversion Funnel", "module": "commerce", "entity": "ShoppingCart", "params": dates()}),
            json!({"name": "coupon_usage", "label": "Coupon Usage", "module": "commerce", "entity": "CouponCode", "params": dates()}),
        ]
    }

    async fn run(&self, k: &Kernel, ctx: &Ctx, name: &str, params: &HashMap<String, String>) -> Result<Value> {
        let p = Params { raw: params };
        let mut tx = read_tx(k, ctx).await?;
        let co = resolve_company(&mut tx, ctx, &p).await?;
        let mut out = match name {
            "web_sales" => web_sales(k, ctx, &mut tx, &co, &p).await?,
            "top_products" => top_products(k, ctx, &mut tx, &co, &p).await?,
            "abandoned_carts" => abandoned(k, ctx, &mut tx, &co, &p).await?,
            "conversion_funnel" => funnel(k, ctx, &mut tx, &co, &p).await?,
            "coupon_usage" => coupons(k, ctx, &mut tx, &co, &p).await?,
            _ => return Err(Error::NotFound(format!("report {name}"))),
        };
        tx.rollback().await?;
        if let Some(m) = out.as_object_mut() {
            m.insert("meta".into(), json!({"company": co.name, "currency": co.currency}));
        }
        Ok(out)
    }
}

/// Restricts `alias` to the rows of `entity` the caller may read.
fn scope(k: &Kernel, ctx: &Ctx, entity: &str, qb: &mut QueryBuilder<Postgres>, alias: &str) -> Result<()> {
    let ent = k.entity(ctx, entity)?;
    row_scope(ctx, &ent, Action::Read)?.push_sql(qb, alias);
    Ok(())
}

fn pct(part: i64, whole: i64) -> Value {
    if whole == 0 { json!(null) } else { money((Decimal::from(part) * Decimal::ONE_HUNDRED / Decimal::from(whole)).round_dp(2)) }
}

/// `from e_shopping_cart c join e_sales_order o ...`: submitted web orders of the company in the period.
fn web_orders(qb: &mut QueryBuilder<Postgres>, ctx: &Ctx, co: &ReportCompany, from: chrono::NaiveDate, to: chrono::NaiveDate) {
    qb.push(" from e_shopping_cart c join e_sales_order o on o.tenant_id = c.tenant_id and o.id = c.sales_order where c.status = 'Ordered' and o.docstatus = 1 and c.tenant_id = ");
    qb.push_bind(ctx.tenant).push(" and o.company = ").push_bind(co.id).push(" and o.posting_date between ").push_bind(from).push(" and ").push_bind(to);
}

/// Excludes the store's shipping line from item figures (`i` is the order line alias).
fn not_shipping(qb: &mut QueryBuilder<Postgres>, ctx: &Ctx) {
    qb.push(" and i.item is distinct from (select shipping_item from e_store_settings where tenant_id = ").push_bind(ctx.tenant).push(" limit 1)");
}

async fn web_sales(k: &Kernel, ctx: &Ctx, tx: &mut Tx, co: &ReportCompany, p: &Params<'_>) -> Result<Value> {
    let (from, to) = period(p, co)?;
    let by = p.str("group_by").unwrap_or("Day");
    let mut qb = QueryBuilder::new("");
    let rows: Vec<(String, i64, Decimal, Decimal)> = match by {
        "Day" => {
            qb.push("select o.posting_date::text, count(distinct o.id), coalesce(sum((select sum(i.qty) from e_sales_order__items i where i.tenant_id = o.tenant_id and i.parent_id = o.id");
            not_shipping(&mut qb, ctx);
            qb.push(")), 0), coalesce(sum(o.base_grand_total), 0)");
            web_orders(&mut qb, ctx, co, from, to);
            scope(k, ctx, "SalesOrder", &mut qb, "o")?;
            qb.push(" group by o.posting_date order by o.posting_date");
            qb.build_query_as().fetch_all(&mut **tx).await?
        }
        "Item" | "Category" => return web_sales_lines(k, ctx, tx, co, from, to, by).await,
        other => return Err(Error::field("group_by", format!("unknown grouping '{other}'"))),
    };
    let (mut orders, mut units, mut revenue) = (0i64, Decimal::ZERO, Decimal::ZERO);
    let out: Vec<Value> = rows
        .into_iter()
        .map(|(day, n, u, r)| {
            orders += n;
            units += u;
            revenue += r;
            json!({"day": day, "orders": n, "units": money(u), "revenue": money(r)})
        })
        .collect();
    Ok(json!({
        "columns": [col("day", "Day", "date"), col("orders", "Orders", "int"), col("units", "Units", "decimal"), col("revenue", "Revenue", "currency")],
        "rows": out,
        "totals": {"orders": orders, "units": money(units), "revenue": money(revenue)},
    }))
}

async fn web_sales_lines(k: &Kernel, ctx: &Ctx, tx: &mut Tx, co: &ReportCompany, from: chrono::NaiveDate, to: chrono::NaiveDate, by: &str) -> Result<Value> {
    let key = if by == "Item" { "coalesce(it.item_name, it.item_code)" } else { "coalesce(ic.category_name, '(no category)')" };
    let mut qb = QueryBuilder::new(format!(
        "select {key}, count(distinct o.id), coalesce(sum(i.qty), 0), coalesce(sum(i.net_amount * coalesce(o.conversion_rate, 1)), 0) \
         from e_shopping_cart c join e_sales_order o on o.tenant_id = c.tenant_id and o.id = c.sales_order \
         join e_sales_order__items i on i.tenant_id = o.tenant_id and i.parent_id = o.id join e_item it on it.tenant_id = i.tenant_id and it.id = i.item"
    ));
    if by == "Category" {
        qb.push(
            " left join e_website_item w on w.tenant_id = i.tenant_id and w.item = i.item \
             left join e_website_item__categories wc on wc.tenant_id = w.tenant_id and wc.parent_id = w.id \
             left join e_item_category ic on ic.tenant_id = wc.tenant_id and ic.id = wc.category",
        );
    }
    qb.push(" where c.status = 'Ordered' and o.docstatus = 1 and c.tenant_id = ").push_bind(ctx.tenant);
    qb.push(" and o.company = ").push_bind(co.id).push(" and o.posting_date between ").push_bind(from).push(" and ").push_bind(to);
    not_shipping(&mut qb, ctx);
    scope(k, ctx, "SalesOrder", &mut qb, "o")?;
    qb.push(format!(" group by {key} order by 4 desc, 1 limit 10000"));
    let rows: Vec<(String, i64, Decimal, Decimal)> = qb.build_query_as().fetch_all(&mut **tx).await?;
    let label = if by == "Item" { "Item" } else { "Category" };
    let (mut units, mut revenue) = (Decimal::ZERO, Decimal::ZERO);
    let out: Vec<Value> = rows
        .into_iter()
        .map(|(name, n, u, r)| {
            units += u;
            revenue += r;
            json!({"name": name, "orders": n, "units": money(u), "revenue": money(r.round_dp(co.precision))})
        })
        .collect();
    Ok(json!({
        "columns": [col("name", label, "data"), col("orders", "Orders", "int"), col("units", "Units", "decimal"), col("revenue", "Revenue (net)", "currency")],
        "rows": out,
        "totals": {"units": money(units), "revenue": money(revenue.round_dp(co.precision))},
    }))
}

async fn top_products(k: &Kernel, ctx: &Ctx, tx: &mut Tx, co: &ReportCompany, p: &Params<'_>) -> Result<Value> {
    let (from, to) = period(p, co)?;
    let mut qb = QueryBuilder::new(
        "select it.item_code, coalesce(max(w.title), it.item_name), count(distinct o.id), coalesce(sum(i.qty), 0), \
         coalesce(sum(i.net_amount * coalesce(o.conversion_rate, 1)), 0) from e_shopping_cart c \
         join e_sales_order o on o.tenant_id = c.tenant_id and o.id = c.sales_order \
         join e_sales_order__items i on i.tenant_id = o.tenant_id and i.parent_id = o.id join e_item it on it.tenant_id = i.tenant_id and it.id = i.item \
         left join e_website_item w on w.tenant_id = i.tenant_id and w.item = i.item where c.status = 'Ordered' and o.docstatus = 1 and c.tenant_id = ",
    );
    qb.push_bind(ctx.tenant).push(" and o.company = ").push_bind(co.id).push(" and o.posting_date between ").push_bind(from).push(" and ").push_bind(to);
    not_shipping(&mut qb, ctx);
    scope(k, ctx, "SalesOrder", &mut qb, "o")?;
    qb.push(" group by it.item_code, it.item_name order by 5 desc, 4 desc limit 50");
    let rows: Vec<(String, Option<String>, i64, Decimal, Decimal)> = qb.build_query_as().fetch_all(&mut **tx).await?;
    let out: Vec<Value> = rows
        .into_iter()
        .enumerate()
        .map(|(i, (code, title, n, u, r))| json!({"rank": i + 1, "item_code": code, "title": title, "orders": n, "units": money(u), "revenue": money(r.round_dp(co.precision))}))
        .collect();
    Ok(json!({
        "columns": [col("rank", "#", "int"), col("item_code", "Item", "data"), col("title", "Title", "data"), col("orders", "Orders", "int"),
                    col("units", "Units", "decimal"), col("revenue", "Revenue (net)", "currency")],
        "rows": out,
    }))
}

async fn abandoned(k: &Kernel, ctx: &Ctx, tx: &mut Tx, co: &ReportCompany, p: &Params<'_>) -> Result<Value> {
    let (from, to) = period(p, co)?;
    let mut qb = QueryBuilder::new(
        "select c.id, c.email, c.contact_name, c.status, coalesce(c.item_count, 0), coalesce(c.rounded_total, 0), coalesce(c.base_grand_total, 0), \
         to_char(c.last_activity at time zone 'UTC', 'YYYY-MM-DD HH24:MI') from e_shopping_cart c \
         where c.status in ('Abandoned', 'Expired') and coalesce(c.item_count, 0) > 0 and c.tenant_id = ",
    );
    qb.push_bind(ctx.tenant).push(" and c.company = ").push_bind(co.id);
    qb.push(" and c.last_activity >= ").push_bind(from).push("::date and c.last_activity < ").push_bind(to).push("::date + 1");
    scope(k, ctx, "ShoppingCart", &mut qb, "c")?;
    qb.push(" order by c.last_activity desc limit 10000");
    let rows: Vec<(uuid::Uuid, Option<String>, Option<String>, String, Decimal, Decimal, Decimal, Option<String>)> =
        qb.build_query_as().fetch_all(&mut **tx).await?;
    let (mut value, mut contactable) = (Decimal::ZERO, 0i64);
    let count = rows.len() as i64;
    let out: Vec<Value> = rows
        .into_iter()
        .map(|(cart, email, name, status, items, total, base, last)| {
            value += base;
            contactable += i64::from(email.is_some());
            json!({"cart": cart, "email": email, "name": name, "status": status, "items": money(items), "value": money(total), "base_value": money(base),
                   "last_activity": last, "contactable": email.is_some()})
        })
        .collect();
    Ok(json!({
        "columns": [col("email", "Email", "data"), col("name", "Name", "data"), col("status", "Status", "data"), col("items", "Items", "decimal"),
                    col("value", "Cart Value", "currency"), col("base_value", "Value (Company Currency)", "currency"), col("last_activity", "Last Activity", "data"),
                    col("contactable", "Contactable", "check")],
        "rows": out,
        "totals": {"carts": count, "base_value": money(value), "contactable": contactable},
    }))
}

async fn funnel(k: &Kernel, ctx: &Ctx, tx: &mut Tx, co: &ReportCompany, p: &Params<'_>) -> Result<Value> {
    let (from, to) = period(p, co)?;
    let mut qb = QueryBuilder::new(
        "select count(*), count(*) filter (where coalesce(c.item_count, 0) > 0 or c.status = 'Ordered'), count(*) filter (where c.status = 'Ordered'), \
         count(*) filter (where c.status = 'Ordered' and o.docstatus = 1 and (o.ext->>'commerce_payment_status') in ('Paid', 'Not Required')) \
         from e_shopping_cart c left join e_sales_order o on o.tenant_id = c.tenant_id and o.id = c.sales_order where c.tenant_id = ",
    );
    qb.push_bind(ctx.tenant).push(" and c.company = ").push_bind(co.id);
    qb.push(" and c.created_at >= ").push_bind(from).push("::date and c.created_at < ").push_bind(to).push("::date + 1");
    scope(k, ctx, "ShoppingCart", &mut qb, "c")?;
    let (created, with_items, ordered, paid): (i64, i64, i64, i64) = qb.build_query_as().fetch_one(&mut **tx).await?;
    let step = |name: &str, n: i64, prev: i64| json!({"step": name, "carts": n, "of_previous": pct(n, prev), "of_created": pct(n, created)});
    Ok(json!({
        "columns": [col("step", "Step", "data"), col("carts", "Carts", "int"), col("of_previous", "% of Previous Step", "percent"), col("of_created", "% of Created", "percent")],
        "rows": [step("Created", created, created), step("With Items", with_items, created), step("Checked Out", ordered, with_items), step("Paid", paid, ordered)],
    }))
}

async fn coupons(k: &Kernel, ctx: &Ctx, tx: &mut Tx, co: &ReportCompany, p: &Params<'_>) -> Result<Value> {
    let (from, to) = period(p, co)?;
    let mut qb = QueryBuilder::new(
        "select cc.code, cc.disabled, coalesce(cc.used_count, 0), coalesce(cc.max_uses, 0), count(o.id), coalesce(sum(o.base_grand_total), 0), \
         coalesce(sum(c.total_discount * coalesce(o.conversion_rate, 1)), 0) from e_coupon_code cc \
         left join e_shopping_cart c on c.tenant_id = cc.tenant_id and c.coupon = cc.id and c.status = 'Ordered' \
         left join e_sales_order o on o.tenant_id = c.tenant_id and o.id = c.sales_order and o.docstatus = 1 and o.company = ",
    );
    qb.push_bind(co.id).push(" and o.posting_date between ").push_bind(from).push(" and ").push_bind(to);
    qb.push(" where cc.tenant_id = ").push_bind(ctx.tenant);
    scope(k, ctx, "CouponCode", &mut qb, "cc")?;
    qb.push(" group by cc.id, cc.code, cc.disabled, cc.used_count, cc.max_uses order by 5 desc, cc.code limit 10000");
    let rows: Vec<(String, Option<bool>, i64, i64, i64, Decimal, Decimal)> = qb.build_query_as().fetch_all(&mut **tx).await?;
    let out: Vec<Value> = rows
        .into_iter()
        .map(|(code, disabled, used, max, orders, revenue, discount)| {
            json!({"code": code, "orders": orders, "revenue": money(revenue), "discount": money(discount.round_dp(co.precision)), "used_count": used,
                   "max_uses": if max > 0 { json!(max) } else { json!(null) }, "disabled": disabled.unwrap_or(false)})
        })
        .collect();
    Ok(json!({
        "columns": [col("code", "Coupon", "data"), col("orders", "Orders in Period", "int"), col("revenue", "Revenue", "currency"),
                    col("discount", "Discounts Given", "currency"), col("used_count", "Uses (All Time)", "int"), col("max_uses", "Limit", "int"),
                    col("disabled", "Disabled", "check")],
        "rows": out,
    }))
}
