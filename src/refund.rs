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

/// The lines `(document, line, item, qty)` that covered something, less what has gone back against
/// them. Returns do not say which line they undo, so a document's returns of an item come off its
/// lines of that item in turn.
pub(crate) fn rooms(lines: &[(Uuid, Uuid, Uuid, Decimal)], gone: &HashMap<(Uuid, Uuid), Decimal>) -> Vec<Room> {
    let mut gone = gone.clone();
    lines
        .iter()
        .filter_map(|(doc, line, item, qty)| {
            let back = gone.entry((*doc, *item)).or_default();
            let taken = (*back).min(*qty);
            *back -= taken;
            let left = *qty - taken;
            (left > Decimal::ZERO).then_some(Room { doc: *doc, line: *line, qty: left })
        })
        .collect()
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

// ───────────────────────────── reading the books ─────────────────────────────

/// What the order's other refunds hold of its payment.
async fn held(tx: &mut Tx, tenant: Uuid, so: Uuid, except: Option<Uuid>) -> Result<Decimal> {
    Ok(sqlx::query_scalar::<_, Decimal>(sqlx::AssertSqlSafe(format!(
        "select coalesce(sum(amount), 0) from e_order_refund where tenant_id = $1 and sales_order = $2 and status in {HOLDING} and ($3::uuid is null or id <> $3)"
    )))
    .bind(tenant)
    .bind(so)
    .bind(except)
    .fetch_one(&mut **tx)
    .await?)
}

#[derive(Default, Clone, Copy)]
struct Claims {
    all: Decimal,
    restocked: Decimal,
}

/// Units already asked to come back, per order line.
async fn claims(tx: &mut Tx, tenant: Uuid, so: Uuid, except: Option<Uuid>) -> Result<HashMap<String, Claims>> {
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

type Lines = Vec<(Uuid, Uuid, Uuid, Decimal)>;

/// Invoice lines that billed an order line (or the order's lines of one item), newest invoice first,
/// and what credit notes took back of each invoice's items.
async fn invoice_rooms(tx: &mut Tx, tenant: Uuid, so: Uuid, line: Option<Uuid>, item: Option<Uuid>) -> Result<Vec<Room>> {
    let lines: Lines = sqlx::query_as(
        "select i.parent_id, i.id, i.item, i.qty from e_sales_invoice__items i join e_sales_invoice s on s.tenant_id = i.tenant_id and s.id = i.parent_id \
         where i.tenant_id = $1 and s.docstatus = 1 and not coalesce(s.is_return, false) and i.sales_order = $2 \
         and ($3::text is null or i.so_line = $3) and ($4::uuid is null or i.item = $4) order by s.posting_date desc, s.created_at desc, i.idx",
    )
    .bind(tenant)
    .bind(so)
    .bind(line.map(|l| l.to_string()))
    .bind(item)
    .fetch_all(&mut **tx)
    .await?;
    let invoices: Vec<Uuid> = lines.iter().map(|l| l.0).collect();
    let credited: Vec<(Uuid, Uuid, Decimal)> = sqlx::query_as(
        "select s.return_against, c.item, -sum(c.qty) from e_sales_invoice__items c join e_sales_invoice s on s.tenant_id = c.tenant_id and s.id = c.parent_id \
         where c.tenant_id = $1 and s.docstatus = 1 and coalesce(s.is_return, false) and s.return_against = any($2) group by s.return_against, c.item",
    )
    .bind(tenant)
    .bind(&invoices)
    .fetch_all(&mut **tx)
    .await?;
    Ok(rooms(&lines, &credited.into_iter().map(|(si, item, q)| ((si, item), q)).collect()))
}

/// Delivery note lines that delivered an order line, newest note first, less what came back.
async fn delivery_rooms(tx: &mut Tx, tenant: Uuid, so: Uuid, line: Uuid) -> Result<Vec<Room>> {
    let lines: Lines = sqlx::query_as(
        "select i.parent_id, i.id, i.item, i.qty from e_delivery_note__items i join e_delivery_note n on n.tenant_id = i.tenant_id and n.id = i.parent_id \
         where i.tenant_id = $1 and n.docstatus = 1 and not coalesce(n.is_return, false) and i.sales_order = $2 and i.so_line = $3 \
         order by n.posting_date desc, n.created_at desc, i.idx",
    )
    .bind(tenant)
    .bind(so)
    .bind(line.to_string())
    .fetch_all(&mut **tx)
    .await?;
    let notes: Vec<Uuid> = lines.iter().map(|l| l.0).collect();
    let back: Vec<(Uuid, Uuid, Decimal)> = sqlx::query_as(
        "select n.return_against, c.item, sum(c.qty) from e_delivery_note__items c join e_delivery_note n on n.tenant_id = c.tenant_id and n.id = c.parent_id \
         where c.tenant_id = $1 and n.docstatus = 1 and coalesce(n.is_return, false) and n.return_against = any($2) group by n.return_against, c.item",
    )
    .bind(tenant)
    .bind(&notes)
    .fetch_all(&mut **tx)
    .await?;
    Ok(rooms(&lines, &back.into_iter().map(|(n, item, q)| ((n, item), q)).collect()))
}

/// The payment being sent back: the order's paid request and its booked payment entry.
async fn paid_payment(hc: &mut HookCtx<'_>, so: Uuid) -> Result<(Doc, Doc)> {
    let request: Option<Uuid> = sqlx::query_scalar(
        "select r.id from e_payment_request r join e_payment_entry p on p.tenant_id = r.tenant_id and p.id = r.payment_entry \
         where r.tenant_id = $1 and r.sales_order = $2 and r.status = 'Paid' and r.docstatus = 1 and p.docstatus = 1 \
         order by r.paid_at desc nulls last, r.created_at desc limit 1",
    )
    .bind(hc.ctx.tenant)
    .bind(so)
    .fetch_optional(&mut **hc.tx)
    .await?;
    let request = request.ok_or_else(|| Error::invalid("this order has no payment to send back"))?;
    let pr = get(hc.kernel, hc.ctx, hc.tx, "PaymentRequest", request).await?;
    let pe = id(&pr, "payment_entry").ok_or_else(|| Error::invalid("this order has no payment to send back"))?;
    let pe = get(hc.kernel, hc.ctx, hc.tx, "PaymentEntry", pe).await?;
    Ok((pr, pe))
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
    shown: Vec<Value>,
    credit: Vec<(Uuid, Vec<(Uuid, Decimal)>)>,
    restock: Vec<(Uuid, Vec<(Uuid, Decimal)>)>,
}

async fn plan_return(hc: &mut HookCtx<'_>, so: &Doc, asked: &[Asked], shipping: bool, except: Option<Uuid>) -> Result<Plan> {
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
    Ok(Plan { shown, credit: group(credit), restock: group(restock) })
}

/// The credit note for part of an invoice, as a draft: ERP's own `make_credit_note` (which copies the
/// invoice) cut down to the lines and quantities that come back.
async fn credit_draft(hc: &mut HookCtx<'_>, actor: &Ctx, invoice: Uuid, picks: &[(Uuid, Decimal)]) -> Result<Doc> {
    let si = get(hc.kernel, hc.ctx, hc.tx, "SalesInvoice", invoice).await?;
    let made = hc.kernel.run_action_tx(actor, hc.tx, "SalesInvoice", invoice, "make_credit_note", &json!({})).await?;
    let note = made.get("id").and_then(Value::as_str).and_then(|v| v.parse::<Uuid>().ok()).ok_or_else(|| Error::internal("no credit note was made"))?;
    let draft = get(hc.kernel, hc.ctx, hc.tx, "SalesInvoice", note).await?;
    let (from, to) = (rows(&si, "items"), rows(&draft, "items"));
    if from.len() != to.len() {
        return Err(Error::internal("the credit note does not copy the invoice's lines"));
    }
    let kept: Vec<Value> = from
        .iter()
        .zip(&to)
        .filter_map(|(was, now)| {
            let (_, q) = picks.iter().find(|(line, _)| Some(*line) == id(was, "id"))?;
            Some(json!({"id": now.get("id"), "qty": money(-*q)}))
        })
        .collect();
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
        match credit_draft(hc, &actor, *invoice, picks).await {
            Ok(note) => total += d(&note, "rounded_total").abs(),
            Err(e) => {
                failed = Some(e);
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
    let (paid, already) = (d(&pr, "amount"), held(hc.tx, tenant, so_id, None).await?);
    let code = s(&pr, "currency_code").unwrap_or_default().to_string();
    let claimed = claims(hc.tx, tenant, so_id, None).await?;
    let lines: Vec<Value> = rows(&so, "items")
        .iter()
        .filter(|l| store.shipping_item.is_none() || id(l, "item") != store.shipping_item)
        .map(|l| {
            let have = claimed.get(&s(l, "id").unwrap_or_default().to_string()).copied().unwrap_or_default();
            let room = if d(l, "billed_qty").is_zero() { Decimal::ZERO } else { returnable(d(l, "delivered_qty"), d(l, "billed_qty"), have.restocked, have.all) };
            json!({"id": s(l, "id"), "item": s(l, "item"), "name": s(l, "item_name"), "qty": money(d(l, "qty")), "delivered": money(d(l, "delivered_qty")), "invoiced": !d(l, "billed_qty").is_zero(),
                   "returnable": money(room), "rate": money(d(l, "rate"))})
        })
        .collect();
    let charge = match store.shipping_item {
        Some(item) => invoice_rooms(hc.tx, tenant, so_id, None, Some(item)).await?.first().map(|r| r.qty > Decimal::ZERO).unwrap_or(false),
        None => false,
    };
    let started = rows(&so, "items").iter().any(|l| d(l, "delivered_qty") > Decimal::ZERO || d(l, "billed_qty") > Decimal::ZERO);
    let can_cancel = !started && already.is_zero();
    let estimate = if cancel {
        json!({"amount": money(if can_cancel { paid } else { Decimal::ZERO }), "errors": if can_cancel { json!([]) } else { json!(["Part of this order was delivered or invoiced, or its payment is already being sent back."]) }})
    } else if asked.is_empty() {
        Value::Null
    } else {
        match plan_return(hc, &so, asked, shipping, None).await {
            Ok(p) => {
                let amount = estimate(hc, &p).await?;
                json!({"amount": money(amount), "errors": []})
            }
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
    let already = held(hc.tx, hc.ctx.tenant, so_id, None).await?;
    let amount = match s(doc, "kind") {
        Some("Cancellation") => {
            let started = rows(&so, "items").iter().any(|l| d(l, "delivered_qty") > Decimal::ZERO || d(l, "billed_qty") > Decimal::ZERO);
            if started {
                return Err(Error::field("kind", "part of this order was delivered or invoiced: send back the goods instead"));
            }
            if already > Decimal::ZERO {
                return Err(Error::invalid("this payment is already being sent back"));
            }
            set(doc, "lines", Value::Array(vec![]));
            set(doc, "include_shipping", false);
            d(&pr, "amount")
        }
        _ => {
            let asked: Vec<Asked> = asked_lines(doc);
            let plan = plan_return(hc, &so, &asked, b(doc, "include_shipping"), None).await?;
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
            let what = if s(doc, "kind") == Some("Cancellation") { "cancelled order" } else { "return" };
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
                let so = id(doc, "sales_order").ok_or_else(|| Error::internal("refund without order"))?;
                let pr = get(hc.kernel, hc.ctx, hc.tx, "PaymentRequest", id(doc, "payment_request").ok_or_else(|| Error::internal("refund without payment"))?)
                    .await?;
                check_cap(
                    d(&pr, "amount"),
                    held(hc.tx, hc.ctx.tenant, so, Some(refund)).await?,
                    d(doc, "amount"),
                    s(doc, "currency_code").unwrap_or_default(),
                )?;
                let mut v = Doc::new();
                set(&mut v, "status", "Approved");
                set(&mut v, "approved_by", hc.ctx.email.clone());
                set(&mut v, "approved_at", crate::now_str());
                hc.kernel.set_values_tx(&sctx, hc.tx, "OrderRefund", refund, v).await?;
                json!({"status": "Approved"})
            }
            "decline" => {
                person(hc)?;
                if !matches!(status.as_str(), "Requested" | "Refund Failed") {
                    return Err(Error::Conflict(format!("this refund is {status}")));
                }
                let mut v = Doc::new();
                set(&mut v, "status", "Declined");
                set(&mut v, "decision_note", json!(param_text(params, "note", 2000)?));
                hc.kernel.set_values_tx(&sctx, hc.tx, "OrderRefund", refund, v).await?;
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
                refresh_order(hc.kernel, &sctx, hc.tx, id(doc, "sales_order").unwrap_or_default()).await?;
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

// ───────────────────────────── the money is out ─────────────────────────────

/// The refund is done: noted on the refund and its payment, the books follow, the shopper is told.
async fn finish(hc: &mut HookCtx<'_>, doc: &Doc, reference: Option<String>, note: Option<String>) -> Result<Value> {
    let sctx = sys(hc.ctx);
    let refund = id(doc, "id").ok_or_else(|| Error::internal("refund without id"))?;
    let so = id(doc, "sales_order").ok_or_else(|| Error::internal("refund without order"))?;
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
        let request = get(hc.kernel, hc.ctx, hc.tx, "PaymentRequest", pr).await?;
        let mut p = Doc::new();
        set(&mut p, "refunded_amount", money(d(&request, "refunded_amount") + amount));
        hc.kernel.set_values_tx(&sctx, hc.tx, "PaymentRequest", pr, p).await?;
    }
    let fresh = get(hc.kernel, hc.ctx, hc.tx, "OrderRefund", refund).await?;
    let failed = try_books(hc, &fresh).await?;
    refresh_order(hc.kernel, &sctx, hc.tx, so).await?;
    let order = get(hc.kernel, hc.ctx, hc.tx, "SalesOrder", so).await?;
    let event = json!({
        "sales_order": so, "number": s(doc, "order_number"), "refund": refund, "kind": s(doc, "kind"), "amount": money(amount), "currency": s(doc, "currency_code"),
        "gateway": s(doc, "gateway"), "email": s(&order, "commerce_contact_email"), "notify": b(doc, "notify_customer"),
    });
    outbox::enqueue(hc.tx, hc.ctx.tenant, "commerce.order_refunded", Some("SalesOrder"), Some(so), event).await?;
    Ok(json!({"status": "Refunded", "booked": failed.is_none(), "booking_error": failed}))
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
    let so_id = id(refund, "sales_order").ok_or_else(|| Error::internal("refund without order"))?;
    let (_, pe) = paid_payment_of(hc, refund).await?;
    let pe_id = id(&pe, "id").ok_or_else(|| Error::internal("payment without id"))?;
    let mut out = Doc::new();
    if s(refund, "kind") == Some("Cancellation") {
        hc.kernel.cancel_tx(&actor, hc.tx, "PaymentEntry", pe_id, None).await?;
        hc.kernel.cancel_tx(&actor, hc.tx, "SalesOrder", so_id, None).await?;
        return Ok(out);
    }
    let so = get(hc.kernel, hc.ctx, hc.tx, "SalesOrder", so_id).await?;
    let asked = asked_lines(refund);
    let plan = plan_return(hc, &so, &asked, b(refund, "include_shipping"), id(refund, "id")).await?;
    for (note, picks) in &plan.restock {
        let made = return_note(hc, &actor, *note, picks).await?;
        if out.get("delivery_note").is_none() {
            set(&mut out, "delivery_note", s(&made, "id").unwrap_or_default());
        }
    }
    let mut credits = Vec::new();
    for (invoice, picks) in &plan.credit {
        let draft = credit_draft(hc, &actor, *invoice, picks).await?;
        let note = id(&draft, "id").ok_or_else(|| Error::internal("credit note without id"))?;
        hc.kernel.submit_tx(&actor, hc.tx, "SalesInvoice", note, None).await?;
        credits.push(note);
    }
    if let Some(first) = credits.first() {
        set(&mut out, "credit_note", first.to_string());
    }
    let entry = money_out(hc, &actor, refund, &pe, &credits).await?;
    set(&mut out, "journal_entry", entry.to_string());
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
    let remark = format!("Refund {} of web order {}", s(refund, "number").unwrap_or_default(), s(refund, "order_number").unwrap_or_default());
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
    fn what_has_gone_back_comes_off_the_lines_in_turn() {
        let (inv, item) = (u(1), u(9));
        let lines = vec![(inv, u(11), item, n("2")), (inv, u(12), item, n("3")), (u(2), u(21), item, n("4"))];
        let none = HashMap::new();
        assert_eq!(rooms(&lines, &none).iter().map(|r| r.qty).collect::<Vec<_>>(), vec![n("2"), n("3"), n("4")]);
        let gone = HashMap::from([((inv, item), n("3"))]);
        let left = rooms(&lines, &gone);
        assert_eq!(
            left.iter().map(|r| (r.line, r.qty)).collect::<Vec<_>>(),
            vec![(u(12), n("2")), (u(21), n("4"))],
            "two units off the first line, one off the second"
        );
        let all = HashMap::from([((inv, item), n("9"))]);
        assert_eq!(rooms(&lines, &all).len(), 1, "another document's lines are untouched");
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
