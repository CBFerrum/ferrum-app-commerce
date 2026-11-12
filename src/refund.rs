//! Refunds and returns of web orders.
//!
//! A refund is a record, `OrderRefund`, of money going back to the shopper. It is one of two kinds:
//! a *return* (delivered goods come back: the goods go back into stock and the invoice is credited,
//! for the lines and quantities someone chose) or a *cancellation* (nothing shipped: the whole payment
//! goes back and the order is cancelled).
//!
//! The server works out what goes back (`amount`), by making the credit note and reading what it
//! comes to, and never takes the figure from the caller. A person approves it; the gateway the order
//! was paid with carries it out and reports back (`refund_done` / `refund_failed`), and it can never
//! approve one; a person who refunded the money some other way says so (`mark_refunded`). Only once
//! the money is out do the books follow: stock returned by a return delivery note, the invoice
//! credited, the money posted out of the bank against what the credit note left owing. If the books
//! refuse (a closed period, a missing account) the refund stays `Refunded`, because the money has
//! gone, and says why; `post_books` tries again once someone has put that right.

use crate::gift_card::GIFT_CARD;
use crate::payment::{PLUGIN_DOMAIN, param_text, refresh_order};
use crate::store::{Store, is_offline, notify_managers};
use async_trait::async_trait;
use ferrum_app_erp::ledger;
use ferrum_domain::util::{b, company, d, elevated, get, id, money, rows, s, set, sys};
use ferrum_kernel::{Action, Ctx, Doc, DocHook, Error, FieldError, HookCtx, Kernel, Result, Tx, outbox};
use rust_decimal::Decimal;
use serde_json::{Map, Value, json};
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

/// What refunds hold of one payment request (a payment's refunds have no order).
async fn held_payment(tx: &mut Tx, tenant: Uuid, pr: Uuid, except: Option<Uuid>) -> Result<Decimal> {
    Ok(sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "select coalesce(sum(amount), 0) from e_order_refund where tenant_id = $1 and payment_request = $2 and status in {HOLDING} and id is distinct from $3"
    )))
    .bind(tenant)
    .bind(pr)
    .bind(except)
    .fetch_one(&mut **tx)
    .await?)
}

/// Asks for `amount` of a paid payment request to go back, as the app that took it decides (a
/// booking's deposit by its cancellation terms), against `credit_note` when the app credited the
/// invoice. A person still approves it and the gateway carries it out, as for an order's refund.
pub async fn refund_payment(k: &Kernel, ctx: &Ctx, tx: &mut Tx, pr: Uuid, amount: Decimal, credit_note: Option<Uuid>, note: &str) -> Result<Doc> {
    let sctx = sys(ctx);
    let request = k.get_tx(&sctx, tx, "PaymentRequest", pr, true).await?;
    if s(&request, "status") != Some("Paid") {
        return Err(Error::invalid("only a paid request's money goes back"));
    }
    let invoice = id(&request, "sales_invoice").ok_or_else(|| Error::invalid("an order's payment is refunded with a return or a cancellation of the order"))?;
    let code = s(&request, "currency_code").unwrap_or_default().to_string();
    check_cap(d(&request, "amount"), held_payment(tx, ctx.tenant, pr, None).await?, amount, &code)?;
    let refund = json!({
        "company": s(&request, "company"), "sales_invoice": invoice.to_string(), "order_number": s(&request, "order_number"),
        "customer": s(&request, "customer"), "kind": "Payment", "note": note, "notify_customer": true,
        "payment_request": pr.to_string(), "gateway": s(&request, "gateway"), "payment_reference": s(&request, "gateway_reference"),
        "currency": s(&request, "currency"), "currency_code": code, "amount": money(amount), "requested_by": ctx.email.clone(), "booked": false,
        "credit_note": credit_note.map(|c| c.to_string()),
    });
    k.insert_tx(&sctx, tx, "OrderRefund", refund.as_object().cloned().unwrap_or_default()).await
}

/// The refunds that hold part of an order's payment: asked for, approved, failed or done.
const HOLDING: &str = "('Requested', 'Approved', 'Refund Failed', 'Refunded')";

// ───────────────────────────── what can go back (pure) ─────────────────────────────

/// A line of a delivery note or an invoice, and how much of it can still go back.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Room {
    pub doc: Uuid,
    pub line: Uuid,
    pub qty: Decimal,
}

/// Takes `qty` out of the rooms, in order; `None` when they cannot hold it.
pub(crate) fn take(rooms: &[Room], qty: Decimal) -> Option<Vec<Room>> {
    let mut left = qty;
    let mut out = Vec::new();
    for r in rooms {
        if left <= Decimal::ZERO {
            break;
        }
        let q = left.min(r.qty);
        out.push(Room { qty: q, ..r.clone() });
        left -= q;
    }
    (left <= Decimal::ZERO).then_some(out)
}

/// Picks grouped by document, in the order the documents first appear.
pub(crate) fn group(picks: Vec<Room>) -> Vec<(Uuid, Vec<(Uuid, Decimal)>)> {
    let mut out: Vec<(Uuid, Vec<(Uuid, Decimal)>)> = Vec::new();
    for p in picks {
        let at = match out.iter().position(|(doc, _)| *doc == p.doc) {
            Some(at) => at,
            None => {
                out.push((p.doc, Vec::new()));
                out.len() - 1
            }
        };
        match out[at].1.iter_mut().find(|(line, _)| *line == p.line) {
            Some((_, q)) => *q += p.qty,
            None => out[at].1.push((p.line, p.qty)),
        }
    }
    out
}

/// How much of an order line may still come back. A delivery that came back lowers what the order
/// calls delivered, so what was restocked is added again; a credit note leaves what the order calls
/// billed alone; the units already asked for are taken off.
pub(crate) fn returnable(delivered: Decimal, billed: Decimal, restocked: Decimal, claimed: Decimal) -> Decimal {
    ((delivered + restocked).min(billed) - claimed).max(Decimal::ZERO)
}

/// A refund may not take back more than its payment still holds.
pub(crate) fn check_cap(paid: Decimal, held: Decimal, amount: Decimal, code: &str) -> Result<()> {
    if amount <= Decimal::ZERO {
        return Err(Error::invalid("there is nothing to send back"));
    }
    let left = (paid - held).max(Decimal::ZERO);
    if amount > left {
        return Err(Error::invalid(format!("at most {code} {} of this payment can still be sent back", left.normalize())));
    }
    Ok(())
}

/// What an error says, for a person.
fn explain(e: &Error) -> String {
    match e {
        Error::Validation(fields) => fields.iter().map(|f| f.message.clone()).collect::<Vec<_>>().join("; "),
        other => other.to_string(),
    }
}

/// A refusal of ERP's (on the credit note it drafts) as one sentence on the refund, not on fields of
/// a document the person never opened.
pub(crate) fn for_person(e: Error) -> Error {
    match e {
        Error::Validation(_) => Error::invalid(explain(&e)),
        other => other,
    }
}

// ───────────────────────────── reading the books ─────────────────────────────

#[derive(Default, Clone, Copy)]
pub(crate) struct Claims {
    pub all: Decimal,
    pub restocked: Decimal,
}

/// Units already asked to come back, per order line.
pub(crate) async fn claims(tx: &mut Tx, tenant: Uuid, so: Uuid, except: Option<Uuid>) -> Result<HashMap<String, Claims>> {
    let found: Vec<(String, Decimal, Decimal)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "select l.sales_order_line, sum(l.qty), coalesce(sum(l.qty) filter (where r.booked and coalesce(l.restock, false)), 0) \
         from e_order_refund__lines l join e_order_refund r on r.tenant_id = l.tenant_id and r.id = l.parent_id \
         where r.tenant_id = $1 and r.sales_order = $2 and r.status in {HOLDING} and ($3::uuid is null or r.id <> $3) group by l.sales_order_line"
    )))
    .bind(tenant)
    .bind(so)
    .bind(except)
    .fetch_all(&mut **tx)
    .await?;
    Ok(found.into_iter().map(|(line, all, restocked)| (line, Claims { all, restocked })).collect())
}

/// Lines that are left once what came back is taken off (ERP keeps it on each line, `returned_qty`).
async fn left_on(tx: &mut Tx, sql: &str, tenant: Uuid, so: Uuid, line: Option<Uuid>, item: Option<Uuid>) -> Result<Vec<Room>> {
    let found: Vec<(Uuid, Uuid, Decimal)> =
        sqlx::query_as(sqlx::AssertSqlSafe(sql)).bind(tenant).bind(so).bind(line.map(|l| l.to_string())).bind(item).fetch_all(&mut **tx).await?;
    Ok(found.into_iter().map(|(doc, line, qty)| Room { doc, line, qty }).collect())
}

/// Invoice lines that billed an order line (or the order's lines of one item), newest invoice first,
/// with what credit notes have not taken back yet.
async fn invoice_rooms(tx: &mut Tx, tenant: Uuid, so: Uuid, line: Option<Uuid>, item: Option<Uuid>) -> Result<Vec<Room>> {
    left_on(
        tx,
        "select i.parent_id, i.id, i.qty - coalesce(i.returned_qty, 0) from e_sales_invoice__items i join e_sales_invoice s on s.tenant_id = i.tenant_id and s.id = i.parent_id \
         where i.tenant_id = $1 and s.docstatus = 1 and not coalesce(s.is_return, false) and i.sales_order = $2 \
         and ($3::text is null or i.so_line = $3) and ($4::uuid is null or i.item = $4) and i.qty - coalesce(i.returned_qty, 0) > 0 \
         order by s.posting_date desc, s.created_at desc, i.idx",
        tenant,
        so,
        line,
        item,
    )
    .await
}

/// Delivery note lines that delivered an order line, newest note first, less what came back.
async fn delivery_rooms(tx: &mut Tx, tenant: Uuid, so: Uuid, line: Uuid) -> Result<Vec<Room>> {
    left_on(
        tx,
        "select i.parent_id, i.id, i.qty - coalesce(i.returned_qty, 0) from e_delivery_note__items i join e_delivery_note n on n.tenant_id = i.tenant_id and n.id = i.parent_id \
         where i.tenant_id = $1 and n.docstatus = 1 and not coalesce(n.is_return, false) and i.sales_order = $2 and i.so_line = $3 \
         and ($4::uuid is null or i.item = $4) and i.qty - coalesce(i.returned_qty, 0) > 0 order by n.posting_date desc, n.created_at desc, i.idx",
        tenant,
        so,
        Some(line),
        None,
    )
    .await
}

/// The payment being sent back: the order's paid request and its booked payment entry. Of an order
/// paid two ways, a gift card's first while it has room left (money goes back where it came from,
/// the card before the rest), then the latest.
async fn paid_payment(hc: &mut HookCtx<'_>, so: Uuid) -> Result<(Doc, Doc)> {
    let found: Vec<(Uuid, Decimal)> = sqlx::query_as(
        "select r.id, r.amount from e_payment_request r join e_payment_entry p on p.tenant_id = r.tenant_id and p.id = r.payment_entry \
         where r.tenant_id = $1 and r.sales_order = $2 and r.status = 'Paid' and r.docstatus = 1 and p.docstatus = 1 \
         order by r.gateway = $3 desc, r.paid_at desc nulls last, r.created_at desc",
    )
    .bind(hc.ctx.tenant)
    .bind(so)
    .bind(GIFT_CARD)
    .fetch_all(&mut **hc.tx)
    .await?;
    let mut request = found.first().map(|f| f.0);
    for (r, amount) in &found {
        if *amount > held_payment(hc.tx, hc.ctx.tenant, *r, None).await? {
            request = Some(*r);
            break;
        }
    }
    let request = request.ok_or_else(|| Error::invalid("this order has no payment to send back"))?;
    let pr = get(hc.kernel, hc.ctx, hc.tx, "PaymentRequest", request).await?;
    let pe = id(&pr, "payment_entry").ok_or_else(|| Error::invalid("this order has no payment to send back"))?;
    let pe = get(hc.kernel, hc.ctx, hc.tx, "PaymentEntry", pe).await?;
    Ok((pr, pe))
}

/// Whether an order has begun to go out: delivered, or invoiced other than at checkout for a gift card.
fn started(so: &Doc) -> bool {
    let card_invoiced = d(so, "commerce_paid_by_card") > Decimal::ZERO;
    rows(so, "items").iter().any(|l| d(l, "delivered_qty") > Decimal::ZERO || (!card_invoiced && d(l, "billed_qty") > Decimal::ZERO))
}

/// How many ways an order was paid (a gift card and a gateway are two).
async fn payments_made(tx: &mut Tx, tenant: Uuid, so: Uuid) -> Result<i64> {
    Ok(sqlx::query_scalar("select count(*) from e_payment_request where tenant_id = $1 and sales_order = $2 and status = 'Paid' and docstatus = 1")
        .bind(tenant)
        .bind(so)
        .fetch_one(&mut **tx)
        .await?)
}

/// Paid payments of an order that no cancellation yet asks to send back.
async fn unclaimed_payments(tx: &mut Tx, tenant: Uuid, so: Uuid) -> Result<i64> {
    Ok(sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "select count(*) from e_payment_request r where r.tenant_id = $1 and r.sales_order = $2 and r.status = 'Paid' and r.docstatus = 1 \
         and not exists (select 1 from e_order_refund f where f.tenant_id = r.tenant_id and f.payment_request = r.id and f.kind = 'Cancellation' and f.status in {HOLDING})"
    )))
    .bind(tenant)
    .bind(so)
    .fetch_one(&mut **tx)
    .await?)
}

/// The cancellations of an order other than `this` that are not finished: (id, gateway, status).
async fn sibling_cancellations(tx: &mut Tx, tenant: Uuid, so: Uuid, this: Uuid) -> Result<Vec<(Uuid, String, String)>> {
    Ok(sqlx::query_as(
        "select id, coalesce(gateway, ''), status from e_order_refund where tenant_id = $1 and sales_order = $2 and kind = 'Cancellation' and id <> $3 \
         and status in ('Requested', 'Approved', 'Refund Failed', 'Refunded') order by created_at",
    )
    .bind(tenant)
    .bind(so)
    .bind(this)
    .fetch_all(&mut **tx)
    .await?)
}

// ───────────────────────────── planning a return ─────────────────────────────

/// A line someone chose to send back.
#[derive(Debug, Clone)]
struct Asked {
    line: Uuid,
    qty: Decimal,
    restock: bool,
}

fn asked_lines(doc: &Doc) -> Vec<Asked> {
    rows(doc, "lines")
        .iter()
        .filter_map(|r| Some(Asked { line: s(r, "sales_order_line")?.parse().ok()?, qty: d(r, "qty"), restock: b(r, "restock") }))
        .collect()
}

/// What a return does to the books: the invoice lines to credit and the delivery note lines to
/// restock, and the lines to show.
struct Plan {
    /// Kept from what the credit notes credit, per cent (a restocking fee).
    fee: Decimal,
    shown: Vec<Value>,
    credit: Vec<(Uuid, Vec<(Uuid, Decimal)>)>,
    restock: Vec<(Uuid, Vec<(Uuid, Decimal)>)>,
}

async fn plan_return(hc: &mut HookCtx<'_>, so: &Doc, asked: &[Asked], shipping: bool, except: Option<Uuid>, fee: Decimal) -> Result<Plan> {
    let tenant = hc.ctx.tenant;
    let so_id = id(so, "id").ok_or_else(|| Error::internal("order without id"))?;
    if asked.is_empty() {
        return Err(Error::field("lines", "choose what comes back"));
    }
    let claimed = claims(hc.tx, tenant, so_id, except).await?;
    let order_lines = rows(so, "items");
    let (mut errors, mut shown, mut credit, mut restock) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let mut seen = HashSet::new();
    for (i, a) in asked.iter().enumerate() {
        let bad = |m: String| FieldError::new("qty", m).in_row("lines", i);
        let Some(row) = order_lines.iter().find(|r| id(r, "id") == Some(a.line)) else {
            errors.push(FieldError::new("sales_order_line", "is not a line of this order").in_row("lines", i));
            continue;
        };
        if !seen.insert(a.line) {
            errors.push(FieldError::new("sales_order_line", "is listed twice").in_row("lines", i));
            continue;
        }
        let name = s(row, "item_name").unwrap_or("this item").to_string();
        let have = claimed.get(&a.line.to_string()).copied().unwrap_or_default();
        let (delivered, billed) = (d(row, "delivered_qty"), d(row, "billed_qty"));
        if delivered.is_zero() {
            errors.push(bad(format!("{name} was not delivered; cancel the order instead")));
            continue;
        }
        if billed.is_zero() {
            errors.push(bad(format!("{name} is not invoiced yet: invoice the order first, a return credits an invoice")));
            continue;
        }
        let room = returnable(delivered, billed, have.restocked, have.all);
        if a.qty > room {
            errors.push(bad(if room.is_zero() {
                format!("{name} has all come back already")
            } else {
                format!("at most {} of {name} can still come back", room.normalize())
            }));
            continue;
        }
        let Some(from_invoices) = take(&invoice_rooms(hc.tx, tenant, so_id, Some(a.line), None).await?, a.qty) else {
            errors.push(bad(format!("the invoices do not cover {} of {name}", a.qty.normalize())));
            continue;
        };
        credit.extend(from_invoices);
        if a.restock {
            match take(&delivery_rooms(hc.tx, tenant, so_id, a.line).await?, a.qty) {
                Some(from_notes) => restock.extend(from_notes),
                None => {
                    errors.push(bad(format!("the deliveries do not cover {} of {name}", a.qty.normalize())));
                    continue;
                }
            }
        }
        shown.push(json!({"id": Uuid::now_v7().to_string(), "sales_order_line": a.line.to_string(), "item": s(row, "item"), "item_name": name, "qty": money(a.qty), "restock": a.restock}));
    }
    if shipping {
        let store = Store::load(hc.kernel, hc.ctx, hc.tx).await?;
        let charge = match store.shipping_item {
            Some(item) => take(&invoice_rooms(hc.tx, tenant, so_id, None, Some(item)).await?, Decimal::ONE),
            None => None,
        };
        match charge {
            Some(picks) => credit.extend(picks),
            None => errors.push(FieldError::new("include_shipping", "there is no delivery charge left to refund on this order")),
        }
    }
    if !errors.is_empty() {
        return Err(Error::Validation(errors));
    }
    Ok(Plan { fee, shown, credit: group(credit), restock: group(restock) })
}

/// The credit note for part of an invoice, as a draft: ERP's own `make_credit_note` (which carries
/// what is still returnable, each line naming the invoice line it gives back in `return_line`) cut
/// down to the lines and quantities that come back.
async fn credit_draft(hc: &mut HookCtx<'_>, actor: &Ctx, invoice: Uuid, picks: &[(Uuid, Decimal)], fee: Decimal) -> Result<Doc> {
    let si = get(hc.kernel, hc.ctx, hc.tx, "SalesInvoice", invoice).await?;
    let made = hc.kernel.run_action_tx(actor, hc.tx, "SalesInvoice", invoice, "make_credit_note", &json!({})).await?;
    let note = made.get("id").and_then(Value::as_str).and_then(|v| v.parse::<Uuid>().ok()).ok_or_else(|| Error::internal("no credit note was made"))?;
    let draft = get(hc.kernel, hc.ctx, hc.tx, "SalesInvoice", note).await?;
    let lines = rows(&draft, "items");
    let mut kept = Vec::new();
    for (line, q) in picks {
        let Some(now) = lines.iter().find(|l| id(l, "return_line") == Some(*line)) else {
            let name = rows(&si, "items").iter().find(|l| id(l, "id") == Some(*line)).and_then(|l| s(l, "item_name").map(str::to_string));
            let number = s(&si, "number").unwrap_or("the invoice");
            return Err(Error::invalid(format!("{} on {number} has already been credited in full", name.as_deref().unwrap_or("a line"))));
        };
        let mut row = json!({"id": now.get("id"), "qty": money(-*q)});
        if fee > Decimal::ZERO {
            // A restocking fee: the note credits what is left of each line's price after it.
            let hundred = Decimal::ONE_HUNDRED;
            row["discount_percent"] = money(hundred - (hundred - d(now, "discount_percent")) * (hundred - fee) / hundred);
        }
        kept.push(row);
    }
    let mut patch = Doc::new();
    set(&mut patch, "items", Value::Array(kept));
    // A fixed charge belongs to the whole invoice, not to a part of it.
    let taxes = rows(&draft, "taxes");
    if taxes.iter().any(|t| s(t, "charge_type") == Some("Actual")) {
        set(&mut patch, "taxes", Value::Array(taxes.into_iter().filter(|t| s(t, "charge_type") != Some("Actual")).map(Value::Object).collect()));
    }
    hc.kernel.update_tx(actor, hc.tx, "SalesInvoice", note, patch, None).await
}

async fn savepoint(tx: &mut Tx) -> Result<()> {
    sqlx::query("savepoint commerce_refund").execute(&mut **tx).await?;
    Ok(())
}

async fn release(tx: &mut Tx) -> Result<()> {
    sqlx::query("release savepoint commerce_refund").execute(&mut **tx).await?;
    Ok(())
}

async fn rollback(tx: &mut Tx) -> Result<()> {
    sqlx::query("rollback to savepoint commerce_refund").execute(&mut **tx).await?;
    release(tx).await
}

/// What the credit notes of a plan come to: made, read, and thrown away.
async fn estimate(hc: &mut HookCtx<'_>, plan: &Plan) -> Result<Decimal> {
    let actor = elevated(hc.ctx);
    savepoint(hc.tx).await?;
    let mut total = Decimal::ZERO;
    let mut failed = None;
    for (invoice, picks) in &plan.credit {
        match credit_draft(hc, &actor, *invoice, picks, plan.fee).await {
            Ok(note) => total += d(&note, "rounded_total").abs(),
            Err(e) => {
                failed = Some(for_person(e));
                break;
            }
        }
    }
    rollback(hc.tx).await?;
    failed.map_or(Ok(total), Err)
}

// ───────────────────────────── what the desk may ask for ─────────────────────────────

/// What a person may send back on an order, and what a choice comes to, for the desk's dialog: the
/// lines that can still come back and how many, the payment and what it still holds, whether the
/// order can be cancelled with a refund instead, and, when lines are chosen, the amount the server
/// would work out. Nothing is written: the credit notes it makes to find the amount are thrown away.
pub async fn plan(k: &Kernel, user: &Ctx, ctx: &Ctx, body: &Map<String, Value>) -> Result<Value> {
    user.require("OrderRefund", Action::Create)?;
    user.require("SalesOrder", Action::Read)?;
    let so = body.get("sales_order").and_then(Value::as_str).and_then(|v| v.parse::<Uuid>().ok()).ok_or_else(|| Error::field("sales_order", "is required"))?;
    let asked: Vec<Asked> = body
        .get("lines")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|l| l.as_object())
                .filter_map(|l| {
                    Some(Asked {
                        line: l.get("sales_order_line")?.as_str()?.parse().ok()?,
                        qty: d(l, "qty"),
                        restock: l.get("restock").and_then(Value::as_bool).unwrap_or(true),
                    })
                })
                .filter(|a| a.qty > Decimal::ZERO)
                .collect()
        })
        .unwrap_or_default();
    let cancel = body.get("kind").and_then(Value::as_str) == Some("Cancellation");
    let shipping = body.get("include_shipping").and_then(Value::as_bool).unwrap_or(false);
    let mut tx = k.db.tenant_tx(ctx.tenant).await?;
    let ent = k.entity(ctx, "OrderRefund")?;
    let mut hc = HookCtx { kernel: k, ctx, tx: &mut tx, entity: &ent };
    let out = plan_in(&mut hc, so, &asked, shipping, cancel).await;
    tx.rollback().await?;
    out
}

async fn plan_in(hc: &mut HookCtx<'_>, so_id: Uuid, asked: &[Asked], shipping: bool, cancel: bool) -> Result<Value> {
    let tenant = hc.ctx.tenant;
    let so = hc.kernel.get_tx(&sys(hc.ctx), hc.tx, "SalesOrder", so_id, false).await?;
    if !b(&so, "commerce_web_order") {
        return Err(Error::NotFound("web order".into()));
    }
    let nothing = |why: &str| Ok(json!({"payable": false, "why": why}));
    if d(&so, "docstatus") != Decimal::ONE {
        return nothing("This order was cancelled.");
    }
    let (pr, _) = match paid_payment(hc, so_id).await {
        Ok(p) => p,
        Err(Error::Invalid(_)) => return nothing("Nothing has been paid on this order, so there is no money to send back."),
        Err(e) => return Err(e),
    };
    let store = Store::load(hc.kernel, hc.ctx, hc.tx).await?;
    let pr_id = id(&pr, "id").unwrap_or_default();
    let (paid, already) = (d(&pr, "amount"), held_payment(hc.tx, tenant, pr_id, None).await?);
    let code = s(&pr, "currency_code").unwrap_or_default().to_string();
    let claimed = claims(hc.tx, tenant, so_id, None).await?;
    let lines: Vec<Value> = rows(&so, "items")
        .iter()
        .filter(|l| store.shipping_item.is_none() || id(l, "item") != store.shipping_item)
        .map(|l| {
            let have = claimed.get(s(l, "id").unwrap_or_default()).copied().unwrap_or_default();
            let room = if d(l, "billed_qty").is_zero() { Decimal::ZERO } else { returnable(d(l, "delivered_qty"), d(l, "billed_qty"), have.restocked, have.all) };
            json!({"id": s(l, "id"), "item": s(l, "item"), "name": s(l, "item_name"), "qty": money(d(l, "qty")), "delivered": money(d(l, "delivered_qty")), "invoiced": !d(l, "billed_qty").is_zero(),
                   "returnable": money(room), "rate": money(d(l, "rate"))})
        })
        .collect();
    let charge = match store.shipping_item {
        Some(item) => invoice_rooms(hc.tx, tenant, so_id, None, Some(item)).await?.first().map(|r| r.qty > Decimal::ZERO).unwrap_or(false),
        None => false,
    };
    let can_cancel = !started(&so) && unclaimed_payments(hc.tx, tenant, so_id).await? > 0;
    let estimate = if cancel {
        json!({"amount": money(if can_cancel { paid } else { Decimal::ZERO }), "errors": if can_cancel { json!([]) } else { json!(["Part of this order was delivered or invoiced, or its payment is already being sent back."]) }})
    } else if asked.is_empty() {
        Value::Null
    } else {
        match plan_return(hc, &so, asked, shipping, None, Decimal::ZERO).await {
            Ok(p) => match estimate(hc, &p).await {
                Ok(amount) => json!({"amount": money(amount), "errors": []}),
                Err(Error::Invalid(why)) => json!({"amount": money(Decimal::ZERO), "errors": [why]}),
                Err(e) => return Err(e),
            },
            Err(Error::Validation(f)) => json!({"amount": money(Decimal::ZERO), "errors": f.iter().map(|x| x.message.clone()).collect::<Vec<_>>()}),
            Err(e) => return Err(e),
        }
    };
    Ok(json!({
        "payable": true,
        "currency": code,
        "payment": {"gateway": s(&pr, "gateway"), "label": store.method(s(&pr, "gateway").unwrap_or_default()).map(|m| m.label.clone()), "reference": s(&pr, "gateway_reference"),
                    "paid": money(paid), "held": money(already), "refundable": money((paid - already).max(Decimal::ZERO)), "offline": is_offline(s(&pr, "gateway").unwrap_or_default())},
        "cancel": {"allowed": can_cancel},
        "lines": lines,
        "shipping": {"available": charge},
        "estimate": estimate,
    }))
}

// ───────────────────────────── asking for a refund ─────────────────────────────

pub struct OrderRefundHook;

/// Fills a new refund: the order, the payment, the lines and what they come to.
async fn work_out(hc: &mut HookCtx<'_>, doc: &mut Doc) -> Result<()> {
    // The order is read with engine rights below: only someone who may read orders asks about one.
    hc.ctx.require("SalesOrder", Action::Read)?;
    let so_id = id(doc, "sales_order").ok_or_else(|| Error::field("sales_order", "is required"))?;
    // Locked, so two refunds of one order queue up and the second counts the first.
    let so = hc.kernel.get_tx(&sys(hc.ctx), hc.tx, "SalesOrder", so_id, true).await?;
    if !b(&so, "commerce_web_order") {
        return Err(Error::field("sales_order", "only web orders are refunded here"));
    }
    if d(&so, "docstatus") != Decimal::ONE {
        return Err(Error::field("sales_order", "this order was cancelled"));
    }
    let (pr, _) = paid_payment(hc, so_id).await?;
    let code = s(&pr, "currency_code").unwrap_or_default().to_string();
    let already = held_payment(hc.tx, hc.ctx.tenant, id(&pr, "id").unwrap_or_default(), None).await?;
    let amount = match s(doc, "kind") {
        Some("Cancellation") => {
            if started(&so) {
                return Err(Error::field("kind", "part of this order was delivered or invoiced: send back the goods instead"));
            }
            // An order paid two ways (a gift card and another) is cancelled by one refund for each.
            if unclaimed_payments(hc.tx, hc.ctx.tenant, so_id).await? == 0 {
                return Err(Error::invalid("this payment is already being sent back"));
            }
            set(doc, "lines", Value::Array(vec![]));
            set(doc, "include_shipping", false);
            d(&pr, "amount")
        }
        _ => {
            let asked: Vec<Asked> = asked_lines(doc);
            let plan = plan_return(hc, &so, &asked, b(doc, "include_shipping"), None, d(doc, "restocking_fee_percent")).await?;
            let amount = estimate(hc, &plan).await?;
            let lines: Vec<Value> = plan.shown;
            set(doc, "lines", Value::Array(lines));
            amount
        }
    };
    check_cap(d(&pr, "amount"), already, amount, &code)?;
    set(doc, "company", s(&so, "company").unwrap_or_default());
    set(doc, "order_number", s(&so, "number").unwrap_or_default());
    set(doc, "customer", json!(s(&so, "customer")));
    set(doc, "payment_request", s(&pr, "id").unwrap_or_default());
    set(doc, "gateway", s(&pr, "gateway").unwrap_or_default());
    set(doc, "payment_reference", json!(s(&pr, "gateway_reference")));
    set(doc, "currency", json!(s(&pr, "currency")));
    set(doc, "currency_code", code);
    set(doc, "amount", money(amount));
    set(doc, "requested_by", hc.ctx.email.clone());
    set(doc, "booked", false);
    Ok(())
}

#[async_trait]
impl DocHook for OrderRefundHook {
    async fn before_validate(&self, hc: &mut HookCtx<'_>, doc: &mut Doc) -> Result<()> {
        // Worked out once, when it is asked for; after that the server's figures stand.
        if id(doc, "payment_request").is_some() {
            return Ok(());
        }
        if s(doc, "kind") == Some("Payment") {
            return Err(Error::field("kind", "a payment's refund is asked for by the app that took the payment"));
        }
        work_out(hc, doc).await
    }

    async fn validate_change(&self, _hc: &mut HookCtx<'_>, old: Option<&Doc>, doc: &Doc) -> Result<()> {
        let Some(old) = old else { return Ok(()) };
        let lines = |d: &Doc| {
            rows(d, "lines")
                .iter()
                .map(|r| (s(r, "sales_order_line").map(str::to_string), r.get("qty").map(Value::to_string), b(r, "restock")))
                .collect::<Vec<_>>()
        };
        if ["sales_order", "kind", "reason", "include_shipping"].iter().any(|f| old.get(*f) != doc.get(*f)) || lines(old) != lines(doc) {
            return Err(Error::invalid("a refund cannot be changed once it is asked for: decline it and ask again"));
        }
        Ok(())
    }

    async fn after_save(&self, hc: &mut HookCtx<'_>, doc: &Doc, old: Option<&Doc>) -> Result<()> {
        if old.is_none()
            && let Some(refund) = id(doc, "id")
        {
            let what = match s(doc, "kind") {
                Some("Cancellation") => "cancelled order",
                Some("Payment") => "payment",
                Some("Adjustment") => "changed order",
                _ => "return",
            };
            let body = format!(
                "{} · {what} · {} {}",
                s(doc, "order_number").unwrap_or_default(),
                s(doc, "currency_code").unwrap_or_default(),
                d(doc, "amount").normalize()
            );
            notify_managers(hc.tx, hc.ctx.tenant, "A refund needs a decision", Some(&body), "OrderRefund", refund).await?;
        }
        Ok(())
    }

    async fn action(&self, hc: &mut HookCtx<'_>, name: &str, doc: &Doc, params: &Value) -> Result<Option<Value>> {
        let status = s(doc, "status").unwrap_or_default().to_string();
        let refund = id(doc, "id").ok_or_else(|| Error::internal("refund without id"))?;
        let sctx = sys(hc.ctx);
        Ok(Some(match name {
            "approve_refund" => {
                person(hc)?;
                if !matches!(status.as_str(), "Requested" | "Refund Failed") {
                    return Err(Error::Conflict(format!("this refund is {status}")));
                }
                // Another refund may have been asked for since: the payment must still hold this one.
                let pr_id = id(doc, "payment_request").ok_or_else(|| Error::internal("refund without payment"))?;
                let pr = get(hc.kernel, hc.ctx, hc.tx, "PaymentRequest", pr_id).await?;
                let already = held_payment(hc.tx, hc.ctx.tenant, pr_id, Some(refund)).await?;
                check_cap(d(&pr, "amount"), already, d(doc, "amount"), s(doc, "currency_code").unwrap_or_default())?;
                let mut v = Doc::new();
                set(&mut v, "status", "Approved");
                set(&mut v, "approved_by", hc.ctx.email.clone());
                set(&mut v, "approved_at", crate::now_str());
                hc.kernel.set_values_tx(&sctx, hc.tx, "OrderRefund", refund, v).await?;
                // No gateway carries a gift card's refund out: it goes back onto the card now, unless the
                // order's other payment is still on its way back: that goes through a provider and can fail,
                // so the card is given its value back only once it has gone (see `finish`).
                if s(doc, "gateway") == Some(GIFT_CARD) || s(doc, "destination") == Some("Store Credit") {
                    if s(doc, "kind") == Some("Cancellation")
                        && let Some(so) = id(doc, "sales_order")
                        && sibling_cancellations(hc.tx, hc.ctx.tenant, so, refund).await?.iter().any(|(_, g, st)| g != GIFT_CARD && st != "Refunded")
                    {
                        return Ok(Some(json!({"status": "Approved", "waiting": "the order's other payment is being sent back first"})));
                    }
                    let approved = get(hc.kernel, hc.ctx, hc.tx, "OrderRefund", refund).await?;
                    return finish(hc, &approved, None, None).await.map(Some);
                }
                json!({"status": "Approved"})
            }
            "decline" => {
                person(hc)?;
                if !matches!(status.as_str(), "Requested" | "Refund Failed") {
                    return Err(Error::Conflict(format!("this refund is {status}")));
                }
                let note = param_text(params, "note", 2000)?;
                // A cancellation of an order paid two ways is one decision: declined for one payment, it is declined
                // for the other, unless that has already gone back (the order then stays cancelled by hand).
                let mut also = Vec::new();
                if s(doc, "kind") == Some("Cancellation")
                    && let Some(so) = id(doc, "sales_order")
                {
                    for (sib, _, st) in sibling_cancellations(hc.tx, hc.ctx.tenant, so, refund).await? {
                        if st == "Refunded" {
                            return Err(Error::invalid("the order's other payment has already gone back: finish this one too, or refund it another way"));
                        }
                        also.push(sib);
                    }
                }
                for r in std::iter::once(refund).chain(also) {
                    let mut v = Doc::new();
                    set(&mut v, "status", "Declined");
                    set(&mut v, "decision_note", json!(note));
                    hc.kernel.set_values_tx(&sctx, hc.tx, "OrderRefund", r, v).await?;
                }
                json!({"status": "Declined"})
            }
            "mark_refunded" => {
                person(hc)?;
                if !matches!(status.as_str(), "Requested" | "Approved" | "Refund Failed") {
                    return Err(Error::Conflict(format!("this refund is {status}")));
                }
                let note = param_text(params, "note", 2000)?.ok_or_else(|| Error::field("note", "say how it was refunded"))?;
                finish(hc, doc, param_text(params, "refund_reference", 200)?, Some(note)).await?
            }
            "refund_done" | "refund_failed" => {
                the_gateway(hc.ctx, doc)?;
                // Repeated by a gateway that did not hear the first answer: no change.
                if (status == "Refunded" && name == "refund_done") || (status == "Refund Failed" && name == "refund_failed") {
                    return Ok(Some(json!({})));
                }
                if status != "Approved" {
                    return Err(Error::Conflict(format!("this refund is {status}")));
                }
                if name == "refund_done" {
                    let reference = param_text(params, "refund_reference", 200)?.ok_or_else(|| Error::field("refund_reference", "is required"))?;
                    finish(hc, doc, Some(reference), None).await?
                } else {
                    let mut v = Doc::new();
                    set(&mut v, "status", "Refund Failed");
                    set(
                        &mut v,
                        "refund_error",
                        params.get("error").and_then(Value::as_str).unwrap_or("the gateway refused").chars().take(2000).collect::<String>(),
                    );
                    hc.kernel.set_values_tx(&sctx, hc.tx, "OrderRefund", refund, v).await?;
                    json!({"status": "Refund Failed"})
                }
            }
            "post_books" => {
                person(hc)?;
                if status != "Refunded" || b(doc, "booked") {
                    return Err(Error::Conflict("there is nothing left to post for this refund".into()));
                }
                let failed = try_books(hc, doc).await?;
                if let Some(so) = id(doc, "sales_order") {
                    refresh_order(hc.kernel, &sctx, hc.tx, so).await?;
                }
                if failed.is_none() {
                    release_waiting_card(hc, doc).await?;
                }
                json!({"booked": failed.is_none(), "error": failed})
            }
            _ => return Ok(None),
        }))
    }
}

/// Refunds are decided by people.
fn person(hc: &HookCtx<'_>) -> Result<()> {
    if hc.ctx.email.ends_with(PLUGIN_DOMAIN) {
        return Err(Error::Forbidden("a person decides this, not a plugin".into()));
    }
    Ok(())
}

/// Only the gateway the order was paid with reports a refund as sent or refused.
fn the_gateway(ctx: &Ctx, doc: &Doc) -> Result<()> {
    let gateway = s(doc, "gateway").unwrap_or_default();
    match ctx.email.strip_suffix(PLUGIN_DOMAIN) {
        Some(plugin) if plugin == gateway && !is_offline(gateway) => Ok(()),
        Some(plugin) => Err(Error::Forbidden(format!("plugin '{plugin}' cannot act on refunds of '{gateway}'"))),
        None => Err(Error::Forbidden("this is reported by the gateway".into())),
    }
}

// ───────────────────────────── cancelling an order ─────────────────────────────

/// The reason a cancellation carries when nobody chose one: the seeded one, made when the workspace has
/// not been given it yet.
async fn cancel_reason(hc: &mut HookCtx<'_>) -> Result<Uuid> {
    reason_named(hc, "Cancelled before it shipped").await
}

/// The reason called `name`, made (never restocking) when the workspace has not been given it yet.
pub(crate) async fn reason_named(hc: &mut HookCtx<'_>, name: &str) -> Result<Uuid> {
    let sctx = sys(hc.ctx);
    if let Some(found) =
        ferrum_domain::util::find(hc.kernel, &sctx, hc.tx, "ReturnReason", vec![ferrum_kernel::Filter::eq("reason", name)], None, 1).await?.first()
    {
        return id(found, "id").ok_or_else(|| Error::internal("reason without id"));
    }
    let made = hc.kernel.insert_tx(&sctx, hc.tx, "ReturnReason", json!({"reason": name, "restock": false}).as_object().cloned().unwrap_or_default()).await?;
    id(&made, "id").ok_or_else(|| Error::internal("reason without id"))
}

/// Cancels a web order nothing of which has gone out: one that nothing was paid on is cancelled at once
/// (its requests close and a gift card's held value is let go); a paid one is cancelled by a refund for
/// each payment (a gift card's first), so the card gets its value back and the provider's part goes back
/// through its own path, and the order is cancelled when the last has gone. `approve` decides the refunds
/// as it asks for them: a card alone is then back in this transaction; a provider's refund is carried out by
/// the gateway and can fail, and then it is a refund in the state "Refund Failed" that a person retries.
pub async fn cancel_order(hc: &mut HookCtx<'_>, so: &Doc, reason: Option<Uuid>, note: Option<String>, approve: bool) -> Result<Value> {
    let so_id = id(so, "id").ok_or_else(|| Error::internal("order without id"))?;
    if d(so, "docstatus") != Decimal::ONE {
        return Err(Error::invalid("this order was cancelled already"));
    }
    if started(so) {
        return Err(Error::invalid("part of this order has been delivered or invoiced: it can no longer be cancelled, the goods can be sent back instead"));
    }
    let sctx = sys(hc.ctx);
    let to_send = unclaimed_payments(hc.tx, hc.ctx.tenant, so_id).await?;
    if payments_made(hc.tx, hc.ctx.tenant, so_id).await? == 0 {
        hc.kernel.cancel_tx(&sctx, hc.tx, "SalesOrder", so_id, None).await?;
        return Ok(json!({"cancelled": true, "refunds": []}));
    }
    if to_send == 0 {
        return Err(Error::invalid("this order's payment is already being sent back"));
    }
    let reason = match reason {
        Some(r) => r,
        None => cancel_reason(hc).await?,
    };
    let mut made = Vec::new();
    for _ in 0..to_send {
        let refund = json!({"sales_order": so_id.to_string(), "kind": "Cancellation", "reason": reason.to_string(), "note": note, "notify_customer": true});
        let draft = hc.kernel.insert_tx(&sctx, hc.tx, "OrderRefund", refund.as_object().cloned().unwrap_or_default()).await?;
        made.push(id(&draft, "id").ok_or_else(|| Error::internal("refund without id"))?);
    }
    if approve {
        for r in &made {
            hc.kernel.run_action_tx(&sctx, hc.tx, "OrderRefund", *r, "approve_refund", &json!({})).await?;
        }
    }
    let mut out = Vec::new();
    for r in &made {
        let doc = get(hc.kernel, &sctx, hc.tx, "OrderRefund", *r).await?;
        out.push(json!({"id": r.to_string(), "gateway": s(&doc, "gateway"), "status": s(&doc, "status"), "amount": money(d(&doc, "amount"))}));
    }
    let after = get(hc.kernel, &sctx, hc.tx, "SalesOrder", so_id).await?;
    Ok(json!({"cancelled": d(&after, "docstatus") == Decimal::TWO, "refunds": out}))
}

/// The desk's "Cancel Web Order".
pub async fn web_cancel(hc: &mut HookCtx<'_>, so: &Doc, params: &Value) -> Result<Value> {
    crate::ship::guard(hc, so)?;
    let reason = param_text(params, "reason", 36)?.map(|r| r.parse::<Uuid>().map_err(|_| Error::field("reason", "choose one of the reasons"))).transpose()?;
    let approve = !matches!(param_text(params, "approve", 10)?.as_deref().map(str::to_ascii_lowercase).as_deref(), Some("no"));
    cancel_order(hc, so, reason, param_text(params, "note", 2000)?, approve).await
}

// ───────────────────────────── the money is out ─────────────────────────────

/// The refund is done: noted on the refund and its payment, the books follow, the shopper is told.
async fn finish(hc: &mut HookCtx<'_>, doc: &Doc, reference: Option<String>, note: Option<String>) -> Result<Value> {
    let sctx = sys(hc.ctx);
    let refund = id(doc, "id").ok_or_else(|| Error::internal("refund without id"))?;
    let amount = d(doc, "amount");
    let mut v = Doc::new();
    set(&mut v, "status", "Refunded");
    set(&mut v, "refunded_at", crate::now_str());
    set(&mut v, "refund_error", Value::Null);
    if let Some(r) = reference {
        set(&mut v, "refund_reference", r);
    }
    if let Some(n) = note {
        set(&mut v, "decision_note", n);
    }
    // Refunded by a person without being approved first: that is the approval.
    if s(doc, "approved_by").is_none() && !hc.ctx.email.ends_with(PLUGIN_DOMAIN) {
        set(&mut v, "approved_by", hc.ctx.email.clone());
        set(&mut v, "approved_at", crate::now_str());
    }
    hc.kernel.set_values_tx(&sctx, hc.tx, "OrderRefund", refund, v).await?;
    if let Some(pr) = id(doc, "payment_request") {
        // Locked: two refunds of one payment finishing together each add theirs.
        let request = hc.kernel.get_tx(&sctx, hc.tx, "PaymentRequest", pr, true).await?;
        let mut p = Doc::new();
        set(&mut p, "refunded_amount", money(d(&request, "refunded_amount") + amount));
        hc.kernel.set_values_tx(&sctx, hc.tx, "PaymentRequest", pr, p).await?;
    }
    let fresh = get(hc.kernel, hc.ctx, hc.tx, "OrderRefund", refund).await?;
    let failed = try_books(hc, &fresh).await?;
    match id(doc, "sales_order") {
        Some(so) => {
            refresh_order(hc.kernel, &sctx, hc.tx, so).await?;
            crate::edit::refresh_owed(hc.kernel, &sctx, hc.tx, so).await?;
            let order = get(hc.kernel, hc.ctx, hc.tx, "SalesOrder", so).await?;
            let event = json!({
                "sales_order": so, "number": s(doc, "order_number"), "refund": refund, "kind": s(doc, "kind"), "amount": money(amount), "currency": s(doc, "currency_code"),
                "gateway": s(doc, "gateway"), "email": s(&order, "commerce_contact_email"), "notify": b(doc, "notify_customer"),
            });
            outbox::enqueue(hc.tx, hc.ctx.tenant, "commerce.order_refunded", Some("SalesOrder"), Some(so), event).await?;
            if b(doc, "notify_customer") {
                let number = s(doc, "order_number").unwrap_or_default();
                let store = Store::load(hc.kernel, &sctx, hc.tx).await?;
                let mut values = serde_json::Map::new();
                values.insert("amount".into(), json!(crate::pages::money(s(doc, "currency_code").unwrap_or_default(), &amount.to_string(), store.precision)));
                values.insert("link".into(), json!(crate::tell::order_link(hc.tx, hc.ctx.tenant, store.store_url.as_deref(), number, so).await?));
                crate::tell::message(hc.kernel, hc.tx, &sctx, "commerce.order_refunded", so, s(&order, "commerce_contact_email"), values, vec![]).await?;
            }
        }
        None => {
            let (pr, _) = paid_payment_of(hc, doc).await?;
            let invoice = id(doc, "sales_invoice");
            let event = json!({
                "sales_invoice": invoice, "number": s(doc, "order_number"), "refund": refund, "amount": money(amount), "currency": s(doc, "currency_code"),
                "gateway": s(doc, "gateway"), "email": s(&pr, "payer_email"), "notify": b(doc, "notify_customer"),
            });
            outbox::enqueue(hc.tx, hc.ctx.tenant, "commerce.payment_refunded", Some("SalesInvoice"), invoice, event).await?;
        }
    }
    if failed.is_none() {
        release_waiting_card(hc, doc).await?;
        if let Some(request) = id(doc, "return_request") {
            crate::shopper::refunded(hc.kernel, hc.ctx, hc.tx, request).await?;
        }
    }
    Ok(json!({"status": "Refunded", "booked": failed.is_none(), "booking_error": failed}))
}

/// The card's part of an order cancelled two ways goes back last, once the provider's part has gone and is
/// booked: the card refund that waited for it is finished now.
async fn release_waiting_card(hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
    let (Some(refund), Some(so)) = (id(doc, "id"), id(doc, "sales_order")) else { return Ok(()) };
    if s(doc, "kind") != Some("Cancellation") || s(doc, "gateway") == Some(GIFT_CARD) {
        return Ok(());
    }
    for (sib, gateway, st) in sibling_cancellations(hc.tx, hc.ctx.tenant, so, refund).await? {
        if gateway == GIFT_CARD && st == "Approved" {
            let waiting = get(hc.kernel, hc.ctx, hc.tx, "OrderRefund", sib).await?;
            Box::pin(finish(hc, &waiting, None, None)).await?;
        }
    }
    Ok(())
}

/// Posts the books for a refunded refund, all or nothing. A refusal leaves it for a person (and
/// tells the managers): the money has gone either way.
async fn try_books(hc: &mut HookCtx<'_>, refund: &Doc) -> Result<Option<String>> {
    let sctx = sys(hc.ctx);
    let id_ = id(refund, "id").ok_or_else(|| Error::internal("refund without id"))?;
    savepoint(hc.tx).await?;
    let outcome = post_books(hc, refund).await;
    let mut v = Doc::new();
    let failed = match outcome {
        Ok(posted) => {
            release(hc.tx).await?;
            v = posted;
            set(&mut v, "booked", true);
            set(&mut v, "booking_error", Value::Null);
            None
        }
        Err(e) => {
            rollback(hc.tx).await?;
            let why = explain(&e).chars().take(1000).collect::<String>();
            set(&mut v, "booked", false);
            set(&mut v, "booking_error", why.clone());
            Some(why)
        }
    };
    hc.kernel.set_values_tx(&sctx, hc.tx, "OrderRefund", id_, v).await?;
    if let Some(why) = &failed {
        let body = format!("{}: {why}", s(refund, "order_number").unwrap_or_default());
        notify_managers(hc.tx, hc.ctx.tenant, "A refund could not be posted to the books", Some(&body), "OrderRefund", id_).await?;
    }
    Ok(failed)
}

/// The books of a refund: stock back, invoice credited, money out; or, for an order cancelled
/// before it shipped, its payment reversed and the order cancelled. Returns the fields to set.
async fn post_books(hc: &mut HookCtx<'_>, refund: &Doc) -> Result<Doc> {
    let actor = elevated(hc.ctx);
    let (_, pe) = paid_payment_of(hc, refund).await?;
    let pe_id = id(&pe, "id").ok_or_else(|| Error::internal("payment without id"))?;
    let mut out = Doc::new();
    // Part of a payment: only the money moves, against the credit note the app made (if any).
    if s(refund, "kind") == Some("Payment") {
        let credits: Vec<Uuid> = id(refund, "credit_note").into_iter().collect();
        let entry = money_out(hc, &actor, refund, &pe, &credits).await?;
        set(&mut out, "journal_entry", entry.to_string());
        return Ok(out);
    }
    let so_id = id(refund, "sales_order").ok_or_else(|| Error::internal("refund without order"))?;
    // What an edit of the order took off it: the payment's surplus goes back, no goods and no credit note.
    if s(refund, "kind") == Some("Adjustment") {
        let entry = money_out(hc, &actor, refund, &pe, &[pe_id]).await?;
        set(&mut out, "journal_entry", entry.to_string());
        return Ok(out);
    }
    if s(refund, "kind") == Some("Cancellation") {
        // A gift card's payment cancelled gives its value back to the card; its checkout invoice goes too.
        hc.kernel.cancel_tx(&actor, hc.tx, "PaymentEntry", pe_id, None).await?;
        // An order paid two ways is cancelled by the last of its payments to go back.
        if payments_made(hc.tx, hc.ctx.tenant, so_id).await? > 0 {
            return Ok(out);
        }
        let invoices: Vec<Uuid> = sqlx::query_scalar(
            "select distinct v.id from e_sales_invoice v join e_sales_invoice__items i on i.tenant_id = v.tenant_id and i.parent_id = v.id \
             where v.tenant_id = $1 and i.sales_order = $2 and v.docstatus = 1 and not coalesce(v.is_return, false)",
        )
        .bind(hc.ctx.tenant)
        .bind(so_id)
        .fetch_all(&mut **hc.tx)
        .await?;
        for si in invoices {
            hc.kernel.cancel_tx(&actor, hc.tx, "SalesInvoice", si, None).await?;
        }
        hc.kernel.cancel_tx(&actor, hc.tx, "SalesOrder", so_id, None).await?;
        return Ok(out);
    }
    let so = get(hc.kernel, hc.ctx, hc.tx, "SalesOrder", so_id).await?;
    let asked = asked_lines(refund);
    let plan = plan_return(hc, &so, &asked, b(refund, "include_shipping"), id(refund, "id"), d(refund, "restocking_fee_percent")).await?;
    for (note, picks) in &plan.restock {
        let made = return_note(hc, &actor, *note, picks).await?;
        if out.get("delivery_note").is_none() {
            set(&mut out, "delivery_note", s(&made, "id").unwrap_or_default());
        }
    }
    let mut credits = Vec::new();
    for (invoice, picks) in &plan.credit {
        let draft = credit_draft(hc, &actor, *invoice, picks, plan.fee).await?;
        let note = id(&draft, "id").ok_or_else(|| Error::internal("credit note without id"))?;
        hc.kernel.submit_tx(&actor, hc.tx, "SalesInvoice", note, None).await?;
        credits.push(note);
    }
    if let Some(first) = credits.first() {
        set(&mut out, "credit_note", first.to_string());
    }
    if s(refund, "destination") == Some("Store Credit") {
        let entry = as_store_credit(hc, &actor, refund, &credits).await?;
        set(&mut out, "payment_entry", entry.to_string());
    } else if s(refund, "gateway") == Some(GIFT_CARD) {
        let entry = onto_card(hc, &actor, refund, &pe, &credits).await?;
        set(&mut out, "payment_entry", entry.to_string());
    } else {
        let entry = money_out(hc, &actor, refund, &pe, &credits).await?;
        set(&mut out, "journal_entry", entry.to_string());
    }
    close_if_done(hc, &actor, so_id, &asked).await?;
    Ok(out)
}

async fn paid_payment_of(hc: &mut HookCtx<'_>, refund: &Doc) -> Result<(Doc, Doc)> {
    let pr = get(hc.kernel, hc.ctx, hc.tx, "PaymentRequest", id(refund, "payment_request").ok_or_else(|| Error::internal("refund without payment"))?).await?;
    let pe = get(hc.kernel, hc.ctx, hc.tx, "PaymentEntry", id(&pr, "payment_entry").ok_or_else(|| Error::internal("payment without entry"))?).await?;
    Ok((pr, pe))
}

/// Delivery note that puts goods back: a return against the note that sent them.
async fn return_note(hc: &mut HookCtx<'_>, actor: &Ctx, note: Uuid, picks: &[(Uuid, Decimal)]) -> Result<Doc> {
    let from = get(hc.kernel, hc.ctx, hc.tx, "DeliveryNote", note).await?;
    let mut input = Doc::new();
    for f in
        ["company", "customer", "currency", "conversion_rate", "price_list", "tax_template", "set_warehouse", "ship_to", "customer_address", "discount_percent"]
    {
        if let Some(v) = from.get(f).filter(|v| !v.is_null()) {
            input.insert(f.to_string(), v.clone());
        }
    }
    set(&mut input, "is_return", true);
    set(&mut input, "return_against", note.to_string());
    let lines: Vec<Value> = rows(&from, "items")
        .iter()
        .filter_map(|l| {
            let (_, q) = picks.iter().find(|(line, _)| Some(*line) == id(l, "id"))?;
            let mut r = Doc::new();
            for f in ["item", "uom", "price_list_rate", "discount_percent", "warehouse", "batch_no", "serial_no", "sales_order", "so_line"] {
                if let Some(v) = l.get(f).filter(|v| !v.is_null()) {
                    r.insert(f.to_string(), v.clone());
                }
            }
            r.insert("qty".into(), money(*q));
            r.insert("return_line".into(), json!(s(l, "id")));
            Some(Value::Object(r))
        })
        .collect();
    set(&mut input, "items", Value::Array(lines));
    let draft = hc.kernel.insert_tx(actor, hc.tx, "DeliveryNote", input).await?;
    let id_ = id(&draft, "id").ok_or_else(|| Error::internal("return without id"))?;
    hc.kernel.submit_tx(actor, hc.tx, "DeliveryNote", id_, None).await
}

/// Posts the refund out of the bank the payment came into, against what the credit notes left
/// owing the shopper, so the shopper's account nets to nothing.
async fn money_out(hc: &mut HookCtx<'_>, actor: &Ctx, refund: &Doc, pe: &Doc, credits: &[Uuid]) -> Result<Uuid> {
    let co = company(hc.kernel, &sys(hc.ctx), hc.tx, id(refund, "company").ok_or_else(|| Error::internal("refund without company"))?).await?;
    if id(pe, "currency") != Some(co.currency_id) {
        return Err(Error::invalid("this payment was taken in another currency than the company's: the money out is posted by hand"));
    }
    let amount = d(refund, "amount");
    let (Some(receivable), Some(bank), Some(customer)) = (id(pe, "party_account"), id(pe, "bank_account"), id(pe, "customer")) else {
        return Err(Error::invalid("the payment names no bank account or receivable account to post the refund against"));
    };
    let of = if id(refund, "sales_order").is_some() { "web order" } else { "the payment of" };
    let remark = format!("Refund {} of {of} {}", s(refund, "number").unwrap_or_default(), s(refund, "order_number").unwrap_or_default());
    let entry = json!({
        "company": co.id, "posting_date": crate::today().to_string(), "remark": remark,
        "accounts": [
            {"account": receivable, "party_type": "Customer", "customer": customer, "debit": money(amount), "remarks": remark},
            {"account": bank, "credit": money(amount), "remarks": remark},
        ],
    });
    let draft = hc.kernel.insert_tx(actor, hc.tx, "JournalEntry", entry.as_object().cloned().unwrap_or_default()).await?;
    let je = id(&draft, "id").ok_or_else(|| Error::internal("journal entry without id"))?;
    let posted = hc.kernel.submit_tx(actor, hc.tx, "JournalEntry", je, None).await?;
    let owed = rows(&posted, "accounts").iter().find(|l| s(l, "party_type") == Some("Customer")).and_then(|l| id(l, "id"));
    let (Some(owed), today) = (owed, crate::today()) else { return Err(Error::internal("the refund's journal entry has no customer line")) };
    let mut left = amount;
    for note in credits {
        if left <= Decimal::ZERO {
            break;
        }
        let Some(item) = ledger::load_open_item(hc.tx, hc.ctx.tenant, *note, true).await? else { continue };
        if item.outstanding >= Decimal::ZERO {
            continue;
        }
        let x = left.min(-item.outstanding);
        ledger::allocate(hc.tx, hc.ctx, co.id, *note, "JournalEntry", je, today, -x, -x).await?;
        ledger::allocate(hc.tx, hc.ctx, co.id, owed, "JournalEntry", je, today, x, x).await?;
        left -= x;
    }
    Ok(je)
}

/// Puts a refund back onto the gift card that paid: a refund payment loading that card (ERP's stored
/// value), allocated to what the credit notes left owing the shopper.
async fn onto_card(hc: &mut HookCtx<'_>, actor: &Ctx, refund: &Doc, pe: &Doc, credits: &[Uuid]) -> Result<Uuid> {
    let account = id(pe, "stored_value_account").ok_or_else(|| Error::internal("a gift card's payment without its card"))?;
    let amount = d(refund, "amount");
    let mut left = amount;
    let mut references = Vec::new();
    for note in credits {
        let Some(item) = ledger::load_open_item(hc.tx, hc.ctx.tenant, *note, true).await? else { continue };
        let x = left.min(-item.outstanding);
        if x > Decimal::ZERO {
            references.push(json!({"invoice": note.to_string(), "allocated_amount": money(x)}));
            left -= x;
        }
    }
    if left > Decimal::ZERO {
        return Err(Error::invalid("the credit notes do not cover this refund: it goes back onto the card by hand"));
    }
    let remark =
        format!("Refund {} of web order {} onto its gift card", s(refund, "number").unwrap_or_default(), s(refund, "order_number").unwrap_or_default());
    let entry = json!({
        "company": s(refund, "company"), "posting_date": crate::today().to_string(), "payment_type": "Pay", "is_refund": true,
        "customer": s(pe, "customer"), "currency": s(pe, "currency"), "paid_amount": money(amount), "references": references,
        "stored_value_account": account.to_string(), "remarks": remark, "commerce_sales_order": s(refund, "sales_order"),
    });
    let draft = hc.kernel.insert_tx(actor, hc.tx, "PaymentEntry", entry.as_object().cloned().unwrap_or_default()).await?;
    let made = id(&draft, "id").ok_or_else(|| Error::internal("payment entry without id"))?;
    hc.kernel.submit_tx(actor, hc.tx, "PaymentEntry", made, None).await?;
    Ok(made)
}

/// A return refunded as store credit: the credit note's credit put onto a new store credit account (ERP's
/// stored value), whose code goes to the shopper once, as a secret value of its message.
async fn as_store_credit(hc: &mut HookCtx<'_>, actor: &Ctx, refund: &Doc, credits: &[Uuid]) -> Result<Uuid> {
    let store = Store::load(hc.kernel, &sys(hc.ctx), hc.tx).await?;
    let kind = store.policy.credit_kind.ok_or_else(|| Error::invalid("the store sets no store credit to refund to: refund it to the payment"))?;
    let note = credits.first().ok_or_else(|| Error::invalid("there is no credit note to put onto store credit"))?;
    let key = format!("commerce:store-credit:{}", id(refund, "id").unwrap_or_default());
    let out = ferrum_app_erp::stored_value::refund(hc.kernel, actor, hc.tx, kind, *note, None, None, Some(d(refund, "amount")), &key).await?;
    let payment =
        out.get("payment").and_then(Value::as_str).and_then(|p| p.parse::<Uuid>().ok()).ok_or_else(|| Error::internal("the store credit was not paid"))?;
    if let (Some(so), Some(code)) = (id(refund, "sales_order"), out.get("code").and_then(Value::as_str)) {
        let order = get(hc.kernel, hc.ctx, hc.tx, "SalesOrder", so).await?;
        let mut values = serde_json::Map::new();
        values.insert("code".into(), json!(code));
        values.insert("amount".into(), json!(crate::pages::money(&store.currency_code, &d(refund, "amount").to_string(), store.precision)));
        values.insert("store".into(), json!(store.name));
        crate::tell::message(
            hc.kernel,
            hc.tx,
            &sys(hc.ctx),
            "commerce.store_credit_issued",
            so,
            s(&order, "commerce_contact_email"),
            values,
            vec!["code".into()],
        )
        .await?;
    }
    Ok(payment)
}

/// Closes the order when nothing is left to ship once the returned goods are counted: otherwise the
/// returned units would show as still to deliver, and hold stock for a delivery nobody will make.
async fn close_if_done(hc: &mut HookCtx<'_>, actor: &Ctx, so_id: Uuid, this: &[Asked]) -> Result<()> {
    let so = get(hc.kernel, hc.ctx, hc.tx, "SalesOrder", so_id).await?;
    if matches!(s(&so, "status"), Some("Closed" | "Cancelled")) {
        return Ok(());
    }
    let before = claims(hc.tx, hc.ctx.tenant, so_id, None).await?;
    let nothing_left = rows(&so, "items").iter().all(|l| {
        let line = id(l, "id").unwrap_or_default();
        let back: Decimal = this.iter().filter(|a| a.line == line && a.restock).map(|a| a.qty).sum();
        // `before` already holds this refund's lines (it is not booked yet, so as claims only).
        let restocked = before.get(&line.to_string()).map(|c| c.restocked).unwrap_or_default() + back;
        d(l, "qty") - d(l, "delivered_qty") <= restocked
    });
    if nothing_left {
        hc.kernel.run_action_tx(actor, hc.tx, "SalesOrder", so_id, "close", &json!({})).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(v: &str) -> Decimal {
        v.parse().unwrap()
    }

    fn u(i: u128) -> Uuid {
        Uuid::from_u128(i)
    }

    #[test]
    fn a_quantity_is_taken_from_the_rooms_in_order() {
        let rs = vec![Room { doc: u(1), line: u(11), qty: n("2") }, Room { doc: u(2), line: u(21), qty: n("5") }];
        let took = take(&rs, n("3")).unwrap();
        assert_eq!(took.iter().map(|r| (r.doc, r.qty)).collect::<Vec<_>>(), vec![(u(1), n("2")), (u(2), n("1"))]);
        assert!(take(&rs, n("7")).is_some() && take(&rs, n("7.5")).is_none(), "seven fit, seven and a half do not");
        assert_eq!(group(took), vec![(u(1), vec![(u(11), n("2"))]), (u(2), vec![(u(21), n("1"))])]);
        let twice = group(vec![Room { doc: u(1), line: u(11), qty: n("1") }, Room { doc: u(1), line: u(11), qty: n("2") }]);
        assert_eq!(twice, vec![(u(1), vec![(u(11), n("3"))])], "the same line twice is one pick");
    }

    #[test]
    fn what_may_still_come_back() {
        // Two delivered and billed, none asked for: both may come back.
        assert_eq!(returnable(n("2"), n("2"), n("0"), n("0")), n("2"));
        // One came back and was restocked (the order now says one delivered): one is left.
        assert_eq!(returnable(n("1"), n("2"), n("1"), n("1")), n("1"));
        // One asked for and not booked yet: one is left.
        assert_eq!(returnable(n("2"), n("2"), n("0"), n("1")), n("1"));
        // Billed less than delivered: only what was billed can be credited.
        assert_eq!(returnable(n("3"), n("2"), n("0"), n("0")), n("2"));
        // Never below nothing.
        assert_eq!(returnable(n("1"), n("1"), n("0"), n("5")), Decimal::ZERO);
    }

    #[test]
    fn a_refund_may_not_take_more_than_the_payment_holds() {
        assert!(check_cap(n("100"), n("0"), n("100"), "INR").is_ok());
        assert!(check_cap(n("100"), n("40"), n("60"), "INR").is_ok());
        let over = check_cap(n("100"), n("40"), n("60.01"), "INR").unwrap_err().to_string();
        assert!(over.contains("INR 60"), "{over}");
        assert!(check_cap(n("100"), n("0"), Decimal::ZERO, "INR").is_err(), "nothing to send back");
        assert!(check_cap(n("100"), n("130"), n("1"), "INR").is_err(), "nothing left at all");
    }

    #[test]
    fn only_the_gateway_of_the_payment_reports_it() {
        let schema = ferrum_meta::Schema { version: 1, fy_start_month: 4, ..Default::default() };
        let ctx = |email: &str| {
            let mut c = Ctx::system(Uuid::nil(), std::sync::Arc::new(schema.clone()));
            c.email = email.into();
            c
        };
        let mut refund = Doc::new();
        set(&mut refund, "gateway", "stripe");
        assert!(the_gateway(&ctx("stripe@plugins.ferrum.invalid"), &refund).is_ok());
        assert!(the_gateway(&ctx("paypal@plugins.ferrum.invalid"), &refund).is_err());
        assert!(the_gateway(&ctx("manager@shop.test"), &refund).is_err(), "a person does not report a gateway's refund");
        set(&mut refund, "gateway", "Cash on Delivery");
        assert!(the_gateway(&ctx("cash on delivery@plugins.ferrum.invalid"), &refund).is_err(), "nobody reports an offline refund but a person");
    }
}
