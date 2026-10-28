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
        carts(k, &ctx, &store).await?;
    }
    Ok(())
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
        let open: bool = sqlx::query_scalar(
            "select exists(select 1 from e_payment_request where tenant_id = $1 and sales_order = $2 and docstatus = 1 and status in ('Initiated', 'Paid'))",
        )
        .bind(ctx.tenant)
        .bind(so)
        .fetch_one(&mut *tx)
        .await?;
        if open || d(&order, "docstatus") != Decimal::ONE || s(&order, "commerce_payment_status") == Some("Paid") {
            tx.rollback().await?;
            return Ok(());
        }
        k.cancel_tx(ctx, &mut tx, "SalesOrder", so, None).await?;
        tx.commit().await?;
        Ok(())
    })
    .await
}

/// Marks idle carts abandoned (with an event for contactable ones) and old carts expired.
pub async fn carts(k: &Kernel, ctx: &Ctx, store: &Store) -> Result<(usize, usize)> {
    let mut tx = k.db.tenant_tx(ctx.tenant).await?;
    let abandoned: Vec<(Uuid, Option<String>, Option<String>, Decimal, Decimal)> = sqlx::query_as(
        "update e_shopping_cart c set status = 'Abandoned', version = version + 1, updated_at = now() \
         where c.tenant_id = $1 and c.id in (select id from e_shopping_cart where tenant_id = $1 and status = 'Open' and item_count > 0 \
           and last_activity < now() - make_interval(hours => $2) order by last_activity limit $3) \
         returning c.id, c.email, c.contact_name, coalesce(c.item_count, 0), coalesce(c.rounded_total, 0)",
    )
    .bind(ctx.tenant)
    .bind(store.abandon_after_hours as i32)
    .bind(BATCH)
    .fetch_all(&mut *tx)
    .await?;
    for (cart, email, name, items, total) in abandoned.iter().filter(|a| a.1.is_some()) {
        let event = json!({"cart": cart, "email": email, "name": name, "item_count": items.normalize().to_string(), "grand_total": total.normalize().to_string(),
                           "currency": store.currency_code});
        outbox::enqueue(&mut tx, ctx.tenant, "commerce.cart_abandoned", Some("ShoppingCart"), Some(*cart), event).await?;
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
