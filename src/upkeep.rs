//! Every-minute upkeep for each tenant running the store: unpaid (open or failed) payment
//! requests past their expiry become Expired (and, when the store says so, their orders are cancelled, releasing
//! the stock they reserved); idle carts with items become Abandoned (an event lets a mailer
//! send a reminder), old carts Expired. Each step is idempotent and bounded per run.

use crate::payment::refresh_order;
use crate::store::Store;
use ferrum_domain::util::{d, id, s};
use ferrum_kernel::{Ctx, Error, Kernel, Result, outbox, with_retry};
use rust_decimal::Decimal;
use serde_json::json;
use uuid::Uuid;

const BATCH: i64 = 200;

pub async fn run(k: &Kernel) -> Result<()> {
    let tenants: Vec<Uuid> = {
        let mut tx = k.db.system_tx().await?;
        let t = sqlx::query_scalar("select tenant_id from installed_package where name = 'commerce'").fetch_all(&mut *tx).await?;
        tx.commit().await?;
        t
    };
    for tenant in tenants {
        if let Err(e) = for_tenant(k, tenant).await {
            tracing::warn!(%tenant, "commerce upkeep failed: {e}");
        }
    }
    Ok(())
}

async fn for_tenant(k: &Kernel, tenant: Uuid) -> Result<()> {
    let schema = k.registry.schema(tenant).await?;
    if !schema.packages.iter().any(|p| p == "commerce") {
        return Ok(());
    }
    let mut ctx = Ctx::system(tenant, schema);
    ctx.request_id = format!("commerce-upkeep-{}", Uuid::now_v7());
    let store = {
        let mut tx = k.db.tenant_tx(tenant).await?;
        let store = match Store::load(k, &ctx, &mut tx).await {
            Ok(s) => Some(s),
            Err(Error::NotFound(_)) => None,
            Err(e) => return Err(e),
        };
        tx.commit().await?;
        store
    };
    expire_requests(k, &ctx, store.as_ref()).await?;
    if let Some(store) = store {
        let mut tx = k.db.tenant_tx(tenant).await?;
        crate::store::mark_delivery_charge(&mut tx, tenant, store.shipping_item).await?;
        tx.commit().await?;
        carts(k, &ctx, &store).await?;
        // The shopper's news, each step its own: a failure of one never holds back the others.
        if let Err(e) = carrier_news(k, &ctx, &store).await {
            tracing::warn!(%tenant, "carrier news for shoppers failed: {e}");
        }
        if let Err(e) = stock_alerts(k, &ctx, &store).await {
            tracing::warn!(%tenant, "stock alerts failed: {e}");
        }
    }
    Ok(())
}

/// Tells shoppers what a carrier reports of their parcels, once each: out for delivery (when the carrier's
/// own words for it are in Store Settings) and delivered. Only for deliveries the desk said to tell the shopper
/// of, and only where ERP's carriers and the app's flags on delivery notes are there (an older workspace has
/// neither: nothing is sent).
pub async fn carrier_news(k: &Kernel, ctx: &Ctx, store: &Store) -> Result<usize> {
    let flags =
        ctx.schema.entity("DeliveryNote").is_some_and(|e| e.field("commerce_delivered_notified").is_some() && e.field("commerce_out_notified").is_some());
    if !flags || ctx.schema.entity("CarrierShipment").is_none() {
        return Ok(0);
    }
    let found: Vec<(Uuid, String, Option<String>, Uuid)> = {
        let mut tx = k.db.tenant_tx(ctx.tenant).await?;
        let found = sqlx::query_as(
            "select s.id, s.status, s.tracking_status, n.id from e_carrier_shipment s join e_delivery_note n on n.tenant_id = s.tenant_id and n.id = s.delivery_note \
             where s.tenant_id = $1 and s.status in ('In Transit', 'Delivered') and n.docstatus = 1 and s.last_tracked_at > now() - interval '14 days' \
             and ((s.status = 'Delivered' and not coalesce((n.ext->>'commerce_delivered_notified')::boolean, false)) \
               or (s.status = 'In Transit' and not coalesce((n.ext->>'commerce_out_notified')::boolean, false))) order by s.last_tracked_at desc limit $2",
        )
        .bind(ctx.tenant)
        .bind(BATCH)
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        found
    };
    let mut told = 0;
    for (shipment, status, said, note) in found {
        let delivered = status == "Delivered";
        let out = !delivered && said.as_deref().is_some_and(|w| store.policy.out_for_delivery_words.iter().any(|x| x.eq_ignore_ascii_case(w.trim())));
        if !delivered && !out {
            continue;
        }
        with_retry(|| async {
            let mut tx = k.db.tenant_tx(ctx.tenant).await?;
            let dn = k.get_tx(ctx, &mut tx, "DeliveryNote", note, true).await?;
            let flag = if delivered { "commerce_delivered_notified" } else { "commerce_out_notified" };
            if ferrum_domain::util::b(&dn, flag) {
                tx.rollback().await?;
                return Ok(());
            }
            let ship = k.get_tx(ctx, &mut tx, "CarrierShipment", shipment, false).await?;
            let mut orders: Vec<Uuid> = ferrum_domain::util::rows(&dn, "items").iter().filter_map(|r| id(r, "sales_order")).collect();
            orders.sort();
            orders.dedup();
            for so in orders {
                let order = k.get_tx(ctx, &mut tx, "SalesOrder", so, false).await?;
                if !ferrum_domain::util::b(&order, "commerce_web_order") || !ferrum_domain::util::b(&dn, "commerce_notify") {
                    continue;
                }
                let mut values = serde_json::Map::new();
                values.insert("carrier".into(), json!(s(&dn, "transporter")));
                values.insert("tracking_number".into(), json!(s(&ship, "tracking_number")));
                values.insert("tracking_url".into(), json!(s(&ship, "tracking_url").filter(|u| u.starts_with("https://"))));
                values.insert(
                    "link".into(),
                    json!(crate::tell::order_link(&mut tx, ctx.tenant, store.store_url.as_deref(), s(&order, "number").unwrap_or_default(), so).await?),
                );
                let template = if delivered { "commerce.order_delivered" } else { "commerce.out_for_delivery" };
                crate::tell::message(k, &mut tx, ctx, template, so, s(&order, "commerce_contact_email"), values, vec![]).await?;
            }
            let mut v = ferrum_kernel::Doc::new();
            v.insert(flag.into(), json!(true));
            k.set_values_tx(ctx, &mut tx, "DeliveryNote", note, v).await?;
            tx.commit().await?;
            Ok(())
        })
        .await?;
        told += 1;
    }
    Ok(told)
}

/// Tells each shopper who asked, once, that an item is back: when its stock is above nothing.
pub async fn stock_alerts(k: &Kernel, ctx: &Ctx, store: &Store) -> Result<usize> {
    if ctx.schema.entity("StockAlert").is_none() {
        return Ok(0);
    }
    let waiting: Vec<(Uuid, Uuid, String, String, String)> = {
        let mut tx = k.db.tenant_tx(ctx.tenant).await?;
        let rows = sqlx::query_as(
            "select a.id, w.item, a.email, coalesce(a.item_title, w.title, ''), coalesce(w.slug, '') from e_stock_alert a join e_website_item w on w.tenant_id = a.tenant_id and w.id = a.website_item \
             where a.tenant_id = $1 and a.status = 'Waiting' and w.published order by a.created_at limit $2",
        )
        .bind(ctx.tenant)
        .bind(BATCH)
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        rows
    };
    if waiting.is_empty() {
        return Ok(0);
    }
    let mut told = 0;
    for (alert, item, email, title, slug) in waiting {
        let back = with_retry(|| async {
            let mut tx = k.db.tenant_tx(ctx.tenant).await?;
            let doc = k.get_tx(ctx, &mut tx, "StockAlert", alert, true).await?;
            if s(&doc, "status") != Some("Waiting") {
                tx.rollback().await?;
                return Ok(false);
            }
            let stocked = crate::catalog::stock_items(&mut tx, ctx.tenant, &[item]).await?;
            let have = crate::catalog::available(&mut tx, ctx.tenant, store, &[item]).await?.get(&item).copied().unwrap_or_default();
            if stocked.contains(&item) && have <= Decimal::ZERO {
                tx.rollback().await?;
                return Ok(false);
            }
            let mut values = serde_json::Map::new();
            values.insert("title".into(), json!(title));
            values.insert("link".into(), json!(store.store_url.as_deref().map(|u| format!("{}/p/{}", u.trim_end_matches('/'), slug))));
            crate::tell::about(k, &mut tx, ctx, "commerce.back_in_stock", "StockAlert", alert, Some(&email), values, vec![]).await?;
            let mut v = ferrum_kernel::Doc::new();
            v.insert("status".into(), json!("Sent"));
            v.insert("sent_at".into(), json!(crate::now_str()));
            k.set_values_tx(ctx, &mut tx, "StockAlert", alert, v).await?;
            tx.commit().await?;
            Ok(true)
        })
        .await?;
        told += usize::from(back);
    }
    Ok(told)
}

/// Expires overdue requests one by one (each in its own transaction, re-checked under lock).
pub async fn expire_requests(k: &Kernel, ctx: &Ctx, store: Option<&Store>) -> Result<usize> {
    let due: Vec<Uuid> = {
        let mut tx = k.db.tenant_tx(ctx.tenant).await?;
        let due = sqlx::query_scalar(
            "select id from e_payment_request where tenant_id = $1 and docstatus = 1 and status in ('Initiated', 'Failed') and expires_at <= now() \
             order by expires_at limit $2",
        )
        .bind(ctx.tenant)
        .bind(BATCH)
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        due
    };
    let mut expired = 0;
    for pr in due {
        let order = with_retry(|| async {
            let mut tx = k.db.tenant_tx(ctx.tenant).await?;
            let doc = k.get_tx(ctx, &mut tx, "PaymentRequest", pr, true).await?;
            let overdue = s(&doc, "expires_at").and_then(|t| ferrum_kernel::value::parse_datetime(t).ok()).is_some_and(|t| t <= crate::now());
            if !matches!(s(&doc, "status"), Some("Initiated" | "Failed")) || !overdue {
                tx.rollback().await?;
                return Ok(None);
            }
            let mut v = ferrum_kernel::Doc::new();
            v.insert("status".into(), json!("Expired"));
            k.set_values_tx(ctx, &mut tx, "PaymentRequest", pr, v).await?;
            let so = id(&doc, "sales_order");
            if let Some(so) = so {
                refresh_order(k, ctx, &mut tx, so).await?;
                // The rest of an order a gift card holds part of will not be paid now: the card is let go.
                let still_open: bool = sqlx::query_scalar(
                    "select exists(select 1 from e_payment_request where tenant_id = $1 and sales_order = $2 and docstatus = 1 and status = 'Initiated')",
                )
                .bind(ctx.tenant)
                .bind(so)
                .fetch_one(&mut *tx)
                .await?;
                if let (false, Some(store)) = (still_open, store) {
                    crate::gift_card::unhold(k, ctx, &mut tx, store, so).await?;
                }
                let event = json!({"sales_order": so, "number": s(&doc, "order_number"), "payment_request": pr, "email": s(&doc, "payer_email")});
                outbox::enqueue(&mut tx, ctx.tenant, "commerce.payment_expired", Some("SalesOrder"), Some(so), event).await?;
            }
            tx.commit().await?;
            Ok(so)
        })
        .await?;
        expired += 1;
        // Releasing the stock: cancel the unpaid order (its own transaction; a delivered or
        // invoiced order refuses and stays for staff to settle).
        if let (Some(so), Some(store)) = (order, store)
            && store.cancel_unpaid_orders
            && let Err(e) = cancel_unpaid(k, ctx, so).await
        {
            tracing::info!(%so, "unpaid web order left open: {e}");
        }
    }
    Ok(expired)
}

async fn cancel_unpaid(k: &Kernel, ctx: &Ctx, so: Uuid) -> Result<()> {
    with_retry(|| async {
        let mut tx = k.db.tenant_tx(ctx.tenant).await?;
        let order = k.get_tx(ctx, &mut tx, "SalesOrder", so, true).await?;
        // Still waiting: no payment arrived and no newer request is open in the meantime.
        let (open, by_card, by_other): (bool, Decimal, Decimal) = sqlx::query_as(
            "select coalesce(bool_or(status = 'Initiated'), false), coalesce(sum(amount) filter (where status = 'Paid' and gateway = $3), 0), \
             coalesce(sum(amount) filter (where status = 'Paid' and gateway <> $3), 0) from e_payment_request where tenant_id = $1 and sales_order = $2 and docstatus = 1",
        )
        .bind(ctx.tenant)
        .bind(so)
        .bind(crate::gift_card::GIFT_CARD)
        .fetch_one(&mut *tx)
        .await?;
        if open || d(&order, "docstatus") != Decimal::ONE || by_other > Decimal::ZERO || (by_card.is_zero() && s(&order, "commerce_payment_status") == Some("Paid")) {
            tx.rollback().await?;
            return Ok(());
        }
        if by_card > Decimal::ZERO {
            // An order a gift card paid part of at checkout (invoiced then, by an earlier release): the card
            // gets its value back, which cancels the order.
            let ent = k.entity(ctx, "OrderRefund")?;
            let mut hc = ferrum_kernel::HookCtx { kernel: k, ctx, tx: &mut tx, entity: &ent };
            crate::refund::cancel_order(&mut hc, &order, None, None, true).await?;
        } else {
            k.cancel_tx(ctx, &mut tx, "SalesOrder", so, None).await?;
        }
        tx.commit().await?;
        Ok(())
    })
    .await
}

/// Marks idle carts abandoned (with an event for contactable ones) and old carts expired.
pub async fn carts(k: &Kernel, ctx: &Ctx, store: &Store) -> Result<(usize, usize)> {
    let mut tx = k.db.tenant_tx(ctx.tenant).await?;
    // The shopper's consent to a reminder is recorded at checkout; a workspace from before has no such field.
    let consent =
        if ctx.schema.entity("ShoppingCart").is_some_and(|e| e.field("reminder_consent").is_some()) { "coalesce(c.reminder_consent, false)" } else { "false" };
    let abandoned: Vec<(Uuid, Option<String>, Option<String>, Decimal, Decimal, bool)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "update e_shopping_cart c set status = 'Abandoned', version = version + 1, updated_at = now() \
         where c.tenant_id = $1 and c.id in (select id from e_shopping_cart where tenant_id = $1 and status = 'Open' and item_count > 0 \
           and last_activity < now() - make_interval(hours => $2) order by last_activity limit $3) \
         returning c.id, c.email, c.contact_name, coalesce(c.item_count, 0), coalesce(c.rounded_total, 0), {consent}"
    )))
    .bind(ctx.tenant)
    .bind(store.abandon_after_hours as i32)
    .bind(BATCH)
    .fetch_all(&mut *tx)
    .await?;
    for (cart, email, name, items, total, may_remind) in abandoned.iter().filter(|a| a.1.is_some()) {
        // `consent`: the shopper agreed at checkout to be reminded; other apps (a campaign) act on the event only with it.
        let event = json!({"cart": cart, "email": email, "name": name, "item_count": items.normalize().to_string(), "grand_total": total.normalize().to_string(),
                           "currency": store.currency_code, "consent": may_remind});
        outbox::enqueue(&mut tx, ctx.tenant, "commerce.cart_abandoned", Some("ShoppingCart"), Some(*cart), event).await?;
        if *may_remind {
            let mut values = serde_json::Map::new();
            values.insert("store".into(), json!(store.name));
            values.insert("link".into(), json!(store.store_url.as_deref().map(|u| format!("{}/cart", u.trim_end_matches('/')))));
            crate::tell::about(k, &mut tx, ctx, "commerce.cart_reminder", "ShoppingCart", *cart, email.as_deref(), values, vec![]).await?;
        }
    }
    let expired = sqlx::query(
        "update e_shopping_cart c set status = 'Expired', version = version + 1, updated_at = now() \
         where c.tenant_id = $1 and c.id in (select id from e_shopping_cart where tenant_id = $1 and status in ('Open', 'Abandoned') \
           and coalesce(last_activity, created_at) < now() - make_interval(days => $2) order by last_activity limit $3)",
    )
    .bind(ctx.tenant)
    .bind(store.cart_expiry_days as i32)
    .bind(BATCH)
    .execute(&mut *tx)
    .await?
    .rows_affected() as usize;
    tx.commit().await?;
    Ok((abandoned.len(), expired))
}
