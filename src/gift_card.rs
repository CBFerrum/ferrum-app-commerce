//! Gift cards in the store, as ERP's stored value (the kind in Store Settings' Gift Cards).
//!
//! * **Paying with one.** Stored value pays invoices, not orders: an order a card pays is invoiced at
//!   checkout and ERP redeems the card against that invoice (`stored_value::redeem`, which proves the
//!   code and PIN inside; neither is kept or logged). What the card took is a payment request of its
//!   own, `Gift Card`, already paid, so the order's payments, status and refunds read it as any other;
//!   the rest is asked through the method the shopper chose.
//! * **Selling one.** An item that names a stored value kind is a gift card: once its order is paid,
//!   its lines are invoiced (if they are not yet), which opens an account per card, and each account
//!   is given a new code (ERP's New Code, made there and answered once). The codes go to the shopper in
//!   the `commerce.gift_card_issued` message and nowhere else; the order remembers only that they went.

use crate::store::Store;
use ferrum_app_erp::stored_value;
use ferrum_domain::util::{d, id, money, rows, s, set};
use ferrum_kernel::messages::{self, Message, Recipient};
use ferrum_kernel::{Ctx, Doc, Error, Kernel, Result, Tx};
use rust_decimal::Decimal;
use serde_json::{Map, Value, json};
use uuid::Uuid;

/// What a card's payment request names as its gateway: no plugin has this name, so none settles it.
pub const GIFT_CARD: &str = "Gift Card";

/// A card the shopper gives at checkout.
pub struct Card {
    code: String,
    pin: Option<String>,
}

/// `gift_card: {code, pin?}` from a checkout body.
pub fn parse(body: &Map<String, Value>) -> Result<Option<Card>> {
    let card = match body.get("gift_card") {
        None | Some(Value::Null) => return Ok(None),
        Some(Value::Object(m)) => m,
        Some(_) => return Err(Error::field("gift_card", "must be an object with the card's code")),
    };
    let text = |k: &str| card.get(k).and_then(Value::as_str).map(str::trim).filter(|v| !v.is_empty() && v.len() <= 64).map(str::to_string);
    let code = text("code").ok_or_else(|| Error::field("gift_card.code", "is required"))?;
    Ok(Some(Card { code, pin: text("pin") }))
}

/// What gift cards paid of an order.
pub async fn paid(tx: &mut Tx, tenant: Uuid, so: Uuid) -> Result<Decimal> {
    Ok(sqlx::query_scalar(
        "select coalesce(sum(amount), 0) from e_payment_request where tenant_id = $1 and sales_order = $2 and gateway = $3 and status = 'Paid' and docstatus = 1",
    )
    .bind(tenant)
    .bind(so)
    .bind(GIFT_CARD)
    .fetch_one(&mut **tx)
    .await?)
}

/// A refusal of ERP's about the card, said about the card.
fn about_the_card(e: Error) -> Error {
    match e {
        Error::Invalid(m) => Error::field("gift_card", m),
        Error::Validation(f) => Error::field("gift_card", f.into_iter().map(|x| x.message).collect::<Vec<_>>().join("; ")),
        other => other,
    }
}

/// Invoices an order now and submits the invoice; its id.
async fn invoice(k: &Kernel, ctx: &Ctx, tx: &mut Tx, so: Uuid, only_cards: bool) -> Result<Uuid> {
    let made = k.run_action_tx(ctx, tx, "SalesOrder", so, "make_sales_invoice", &json!({})).await?;
    let si = made.get("id").and_then(Value::as_str).and_then(|v| v.parse().ok()).ok_or_else(|| Error::internal("no invoice was made"))?;
    if only_cards {
        let draft = k.get_tx(ctx, tx, "SalesInvoice", si, false).await?;
        let lines = rows(&draft, "items");
        let cards: Vec<Value> = lines.iter().filter(|l| id(l, "stored_value_kind").is_some()).map(|l| json!({"id": s(l, "id")})).collect();
        if cards.len() < lines.len() {
            let mut patch = Doc::new();
            set(&mut patch, "items", Value::Array(cards));
            k.update_tx(ctx, tx, "SalesInvoice", si, patch, None).await?;
        }
    }
    k.submit_tx(ctx, tx, "SalesInvoice", si, None).await?;
    Ok(si)
}

/// Whether the workspace has the fields a held card part is kept in (an older release of the app
/// has not given them yet: its orders are invoiced and redeemed at checkout, as before).
fn can_hold(ctx: &Ctx) -> bool {
    ctx.schema.entity("SalesOrder").is_some_and(|e| e.field("commerce_card_hold").is_some() && e.field("commerce_card_held").is_some())
}

/// What cards covered of an order: what they paid, and what they hold for it until the rest is paid.
pub async fn covered(k: &Kernel, ctx: &Ctx, tx: &mut Tx, so: Uuid) -> Result<Decimal> {
    let order = k.get_tx(&ferrum_domain::util::sys(ctx), tx, "SalesOrder", so, false).await?;
    Ok(paid(tx, ctx.tenant, so).await?.max(d(&order, "commerce_paid_by_card")) + d(&order, "commerce_card_held"))
}

/// When the value held on an order's card lets go, if any is held.
pub async fn held_until(tx: &mut Tx, tenant: Uuid, so: Uuid) -> Result<Option<chrono::DateTime<chrono::Utc>>> {
    Ok(sqlx::query_scalar("select min(expires_at) from stored_value_hold where tenant_id = $1 and order_id = $2 and state = 'held'")
        .bind(tenant)
        .bind(so)
        .fetch_one(&mut **tx)
        .await?)
}

/// Pays what it can of a just-placed order from `card`, in the checkout's transaction (a wrong code
/// undoes the order with it). Returns what the card paid or holds.
///
/// A card that covers the order pays it now: the order is invoiced and the card redeemed. One that
/// covers only part holds that part for the order (ERP's stored value hold) and nothing is invoiced
/// until the rest is paid ([`settle`]): if the rest is never paid the hold is let go ([`unhold`]),
/// and the order, never invoiced, cancels like any unpaid order.
pub async fn pay(k: &Kernel, ctx: &Ctx, tx: &mut Tx, store: &Store, so: &Doc, card: &Card, key: &str) -> Result<Decimal> {
    let kind = store.gift_card_kind.ok_or_else(|| Error::field("gift_card", "this store takes no gift cards"))?;
    let so_id = id(so, "id").ok_or_else(|| Error::internal("order without id"))?;
    let key = format!("commerce:gift-card:{so_id}:{key}");
    if can_hold(ctx) {
        let have = stored_value::balance(k, ctx, tx, kind, &card.code, card.pin.as_deref()).await.map_err(about_the_card)?;
        let available = have.get("available").and_then(Value::as_str).and_then(|a| a.parse::<Decimal>().ok()).unwrap_or_default();
        if available < d(so, "rounded_total") {
            if available <= Decimal::ZERO {
                return Err(Error::field("gift_card", "this card holds nothing to pay with"));
            }
            let held = stored_value::hold(k, ctx, tx, kind, &card.code, card.pin.as_deref(), so_id, available, &key).await.map_err(about_the_card)?;
            let mut v = Doc::new();
            set(&mut v, "commerce_card_hold", held.get("hold").and_then(Value::as_str).unwrap_or_default());
            set(&mut v, "commerce_card_held", money(available));
            k.set_values_tx(ctx, tx, "SalesOrder", so_id, v).await?;
            return Ok(available);
        }
    }
    let si = invoice(k, ctx, tx, so_id, false).await?;
    let out = stored_value::redeem(k, ctx, tx, kind, &card.code, card.pin.as_deref(), si, None, None, &key).await.map_err(about_the_card)?;
    let amount = out.get("amount").and_then(Value::as_str).and_then(|a| a.parse::<Decimal>().ok()).unwrap_or_default();
    let pe: Uuid = out.get("payment").and_then(Value::as_str).and_then(|p| p.parse().ok()).ok_or_else(|| Error::internal("the card paid nothing"))?;
    record_paid(k, ctx, tx, store, so, amount, pe).await?;
    Ok(amount)
}

/// What the card took, as a paid `Gift Card` payment request on the order (so its payments, status and
/// refunds read it as any other) and the order's own note of it.
async fn record_paid(k: &Kernel, ctx: &Ctx, tx: &mut Tx, store: &Store, so: &Doc, amount: Decimal, pe: Uuid) -> Result<()> {
    let so_id = id(so, "id").ok_or_else(|| Error::internal("order without id"))?;
    let (code, _) = ferrum_domain::util::currency(k, ctx, tx, store.currency).await?;
    let mut pr = Doc::new();
    for (f, v) in [
        ("company", json!(s(so, "company"))),
        ("sales_order", json!(so_id.to_string())),
        ("order_number", json!(s(so, "number"))),
        ("customer", json!(s(so, "customer"))),
        ("payer_name", json!(s(so, "customer_name"))),
        ("payer_email", json!(s(so, "commerce_contact_email"))),
        ("gateway", json!(GIFT_CARD)),
        ("currency", json!(s(so, "currency"))),
        ("currency_code", json!(code)),
        ("amount", money(amount)),
        ("status", json!("Initiated")),
    ] {
        pr.insert(f.into(), v);
    }
    let draft = k.insert_tx(ctx, tx, "PaymentRequest", pr).await?;
    let pr_id = id(&draft, "id").ok_or_else(|| Error::internal("payment request without id"))?;
    k.submit_tx(ctx, tx, "PaymentRequest", pr_id, None).await?;
    let mut v = Doc::new();
    set(&mut v, "status", "Paid");
    set(&mut v, "paid_at", crate::now_str());
    set(&mut v, "payment_entry", pe.to_string());
    k.set_values_tx(ctx, tx, "PaymentRequest", pr_id, v).await?;
    let mut v = Doc::new();
    set(&mut v, "commerce_sales_order", so_id.to_string());
    set(&mut v, "commerce_payment_request", pr_id.to_string());
    k.set_values_tx(ctx, tx, "PaymentEntry", pe, v).await?;
    let mut v = Doc::new();
    set(&mut v, "commerce_paid_by_card", money(amount));
    if can_hold(ctx) {
        set(&mut v, "commerce_card_held", money(Decimal::ZERO));
        set(&mut v, "commerce_card_hold", Value::Null);
    }
    k.set_values_tx(ctx, tx, "SalesOrder", so_id, v).await
}

/// The rest of an order a card holds part of is paid (by a gateway, or on delivery): invoice it, which
/// redeems the held value (ERP's invoice does, once), and record what the card paid. Nothing when no
/// card holds anything or the rest is not paid yet.
pub async fn settle(k: &Kernel, ctx: &Ctx, tx: &mut Tx, store: &Store, so_id: Uuid) -> Result<()> {
    if !can_hold(ctx) {
        return Ok(());
    }
    let so = k.get_tx(ctx, tx, "SalesOrder", so_id, true).await?;
    let held = d(&so, "commerce_card_held");
    if held <= Decimal::ZERO || d(&so, "docstatus") != Decimal::ONE {
        return Ok(());
    }
    let others: Decimal = sqlx::query_scalar(
        "select coalesce(sum(amount), 0) from e_payment_request where tenant_id = $1 and sales_order = $2 and status = 'Paid' and docstatus = 1 and gateway <> $3",
    )
    .bind(ctx.tenant)
    .bind(so_id)
    .bind(GIFT_CARD)
    .fetch_one(&mut **tx)
    .await?;
    if others + held < d(&so, "rounded_total") {
        return Ok(());
    }
    if d(&so, "per_billed") < Decimal::ONE_HUNDRED {
        invoice(k, ctx, tx, so_id, false).await?;
    }
    redeemed(k, ctx, tx, store, so_id).await
}

/// Records the card's part of an order once its held value was redeemed by the order's invoice.
pub async fn redeemed(k: &Kernel, ctx: &Ctx, tx: &mut Tx, store: &Store, so_id: Uuid) -> Result<()> {
    if !can_hold(ctx) {
        return Ok(());
    }
    let so = k.get_tx(ctx, tx, "SalesOrder", so_id, true).await?;
    let Some(hold) = id(&so, "commerce_card_hold").filter(|_| d(&so, "commerce_card_held") > Decimal::ZERO) else { return Ok(()) };
    let pe: Option<Uuid> = sqlx::query_scalar("select payment_id from stored_value_hold where tenant_id = $1 and id = $2 and state = 'redeemed'")
        .bind(ctx.tenant)
        .bind(hold)
        .fetch_optional(&mut **tx)
        .await?
        .flatten();
    let Some(pe) = pe else { return Ok(()) };
    let amount = d(&k.get_tx(ctx, tx, "PaymentEntry", pe, false).await?, "paid_amount");
    record_paid(k, ctx, tx, store, &so, amount, pe).await
}

/// Lets go of what a card holds for an order whose rest will not be paid (its request expired), so the
/// card is the shopper's again. Nothing when nothing is held.
pub async fn unhold(k: &Kernel, ctx: &Ctx, tx: &mut Tx, store: &Store, so_id: Uuid) -> Result<()> {
    if !can_hold(ctx) {
        return Ok(());
    }
    let so = k.get_tx(ctx, tx, "SalesOrder", so_id, true).await?;
    let Some(hold) = id(&so, "commerce_card_hold").filter(|_| d(&so, "commerce_card_held") > Decimal::ZERO) else { return Ok(()) };
    if let Some(kind) = store.gift_card_kind {
        // Already ended (let go by ERP's upkeep, or the order cancelled): only the order's note is left to clear.
        let still: bool = sqlx::query_scalar("select exists(select 1 from stored_value_hold where tenant_id = $1 and id = $2 and state = 'held')")
            .bind(ctx.tenant)
            .bind(hold)
            .fetch_one(&mut **tx)
            .await?;
        if still {
            stored_value::release(k, ctx, tx, kind, hold, &format!("commerce:gift-card-release:{hold}")).await?;
        }
    }
    let mut v = Doc::new();
    set(&mut v, "commerce_card_held", money(Decimal::ZERO));
    set(&mut v, "commerce_card_hold", Value::Null);
    k.set_values_tx(ctx, tx, "SalesOrder", so_id, v).await
}

/// Sends the gift cards a paid order bought, once: their lines invoiced (when they are not yet), a
/// new code for each card the invoices opened, each told to the shopper. Nothing when the order buys
/// none or they went already. The caller holds the order (checkout, or the payment locking it).
pub async fn send_sold(k: &Kernel, ctx: &Ctx, tx: &mut Tx, store: &Store, so_id: Uuid) -> Result<()> {
    let so = k.get_tx(ctx, tx, "SalesOrder", so_id, true).await?;
    if ferrum_domain::util::b(&so, "commerce_cards_sent") {
        return Ok(());
    }
    let items: Vec<Uuid> = rows(&so, "items").iter().filter_map(|l| id(l, "item")).collect();
    let cards: Vec<Uuid> = sqlx::query_scalar("select id from e_item where tenant_id = $1 and id = any($2) and stored_value_kind is not null")
        .bind(ctx.tenant)
        .bind(&items)
        .fetch_all(&mut **tx)
        .await?;
    if cards.is_empty() {
        return Ok(());
    }
    // A code is sealed while its mail waits, and only the server's secret key can: without one the codes
    // are not made (they are shown once), and the managers are told so it can be put right.
    if k.settings.secret_key.is_none() {
        let body = format!("{}: the server has no secret key, so the gift card codes this order bought cannot be sent", s(&so, "number").unwrap_or_default());
        crate::store::notify_managers(tx, ctx.tenant, "Gift card codes were not sent", Some(&body), "SalesOrder", so_id).await?;
        return Ok(());
    }
    if rows(&so, "items").iter().any(|l| id(l, "item").is_some_and(|i| cards.contains(&i)) && d(l, "billed_qty") < d(l, "qty")) {
        invoice(k, ctx, tx, so_id, true).await?;
    }
    let opened: Vec<Uuid> = sqlx::query_scalar(
        "select e.account_id from stored_value_entry e where e.tenant_id = $1 and e.source_entity = 'SalesInvoice' and e.entry_type = 'issue' \
         and e.source_id in (select distinct i.parent_id from e_sales_invoice__items i join e_sales_invoice v on v.tenant_id = i.tenant_id and v.id = i.parent_id \
                             where i.tenant_id = $1 and i.sales_order = $2 and v.docstatus = 1) order by e.created_at",
    )
    .bind(ctx.tenant)
    .bind(so_id)
    .fetch_all(&mut **tx)
    .await?;
    let to = s(&so, "commerce_contact_email").map(Recipient::address);
    for account in opened {
        let made = k.run_action_tx(ctx, tx, "StoredValueAccount", account, "new_code", &json!({})).await?;
        let balance = d(&k.get_tx(ctx, tx, "StoredValueAccount", account, false).await?, "balance");
        let amount = crate::pages::money(&store.currency_code, &balance.to_string(), store.precision);
        let mut values = Map::new();
        values.insert("code".into(), made.get("code").cloned().unwrap_or(Value::Null));
        values.insert("pin".into(), made.get("pin").cloned().unwrap_or(Value::Null));
        values.insert("amount".into(), json!(amount));
        values.insert("store".into(), json!(store.name));
        // The code is a secret value: sealed while the mail waits, blanked once it is handed over.
        let m = Message {
            template: "commerce.gift_card_issued".into(),
            entity: Some("SalesOrder".into()),
            record: Some(so_id),
            to: to.clone().into_iter().collect(),
            values,
            secret: vec!["code".into(), "pin".into()],
            ..Default::default()
        };
        messages::send(k, tx, ctx, &m).await?;
    }
    let mut v = Doc::new();
    set(&mut v, "commerce_cards_sent", true);
    k.set_values_tx(ctx, tx, "SalesOrder", so_id, v).await
}
