// One web order, whole, at /o/{id}: where it stands and the one thing to do next, its items, each
// payment asked of the shopper with what came of it, what shipped and what was invoiced, what was
// sent back, and on the side the customer, where it goes and the story so far. Everything here is
// read from the records as they stand and kept current as they change; what may be done, and what
// each action comes to, is the server's to say.

import { useQuery, useQueryClient } from "@tanstack/react-query";
import { type ReactNode, useCallback, useState } from "react";
import { Link, useParams } from "react-router";
import { ApiError, api, docPath, formatDate, formatDateTime, relativeTime, t as tr } from "@ferrum/client";
import { Alert, Badge, Button, EmptyState, ErrorState, Icon, Menu, MenuItem, PageLoading, statusTone, toast, useLiveEntity, useShellTitle } from "@ferrum/kit";
import { amountText, isOffline, isPaid, nextStep, type Order, statusWords, units, withMethods } from "./queues";
import { copy, f, invoiceOrder, list, markPaid, soft, trackingLink, useRights } from "./data";
import { ExceptionRow, RefundRow } from "./Decisions";
import { type Exception, gatewayName, type Refund } from "./standing";
import { RefundDialog } from "./Refund";
import { ShipDialog } from "./Ship";
import { type AuditEvent, type Delivery, fold, type Invoice, timeline } from "./timeline";

interface Line {
  id: string;
  item?: string | null;
  item_name?: string | null;
  qty?: string | null;
  rate?: string | null;
  amount?: string | null;
  delivered_qty?: string | null;
}

interface Full extends Order {
  version?: number;
  created_at?: string | null;
  items?: Line[];
  taxes?: { description?: string | null; tax_amount?: string | null; deduct?: boolean | null }[];
  total?: string | null;
  total_taxes?: string | null;
  grand_total?: string | null;
  rounding_adjustment?: string | null;
  commerce_notes?: string | null;
  commerce_contact_phone?: string | null;
  commerce_billing_address?: string | null;
}

interface Request {
  id: string;
  number?: string | null;
  gateway?: string | null;
  amount?: string | null;
  currency_code?: string | null;
  status?: string | null;
  expires_at?: string | null;
  paid_at?: string | null;
  payment_url?: string | null;
  gateway_reference?: string | null;
  failure_reason?: string | null;
  refunded_amount?: string | null;
  created_at?: string | null;
  updated_at?: string | null;
}

const REFUND_FIELDS =
  "number,sales_order,order_number,kind,amount,currency_code,gateway,status,requested_by,approved_by,approved_at,refunded_at,refund_reference,refund_error,booked,booking_error,decision_note,note,payment_reference,created_at,updated_at";
const EXCEPTION_FIELDS = "gateway,gateway_reference,payment_request,amount,currency_code,reason,status,approved_by,approved_at,refund_reference,refund_error,note,created_at";

async function related(id: string, signal?: AbortSignal) {
  const mine = f([["sales_order", "=", id]]);
  const through = (extra: unknown[]) => f([["items.sales_order", "=", id], ["docstatus", "=", 1], ...extra]);
  const [payments, deliveries, invoices, refunds, audit] = await Promise.all([
    soft(list<Request>("PaymentRequest", { filters: mine, fields: "number,gateway,amount,currency_code,status,expires_at,paid_at,payment_url,gateway_reference,failure_reason,refunded_amount,created_at,updated_at", sort: "created_at desc", limit: 20 }, signal)),
    soft(list<Delivery & { status?: string }>("DeliveryNote", { filters: through([]), fields: "number,posting_date,is_return,status,transporter,commerce_tracking_number,commerce_tracking_url,created_at", sort: "created_at asc", limit: 20 }, signal)),
    soft(list<Invoice & { outstanding_amount?: string }>("SalesInvoice", { filters: through([]), fields: "number,posting_date,is_return,status,rounded_total,outstanding_amount,created_at", sort: "created_at asc", limit: 20 }, signal)),
    soft(list<Refund>("OrderRefund", { filters: mine, fields: REFUND_FIELDS, sort: "created_at desc", limit: 50, titles: "true" }, signal)),
    // Reading the order does not always include its history.
    api.get<{ events: AuditEvent[] }>(docPath("SalesOrder", id, "audit"), undefined, signal).then((r) => r.events).catch((e) => {
      if (e instanceof ApiError && e.status === 403) return [] as AuditEvent[];
      throw e;
    }),
  ]);
  // Credit notes carry no link to the order, only to the invoice they credit.
  const credits = invoices.length ? await soft(list<Invoice & { outstanding_amount?: string }>("SalesInvoice", { filters: f([["return_against", "in", invoices.map((i) => i.id)], ["docstatus", "=", 1]]), fields: "number,posting_date,is_return,status,rounded_total,outstanding_amount,created_at", sort: "created_at asc", limit: 20 }, signal)) : [];
  const exceptions = payments.length ? await soft(list<Exception>("PaymentException", { filters: f([["payment_request", "in", payments.map((p) => p.id)]]), fields: EXCEPTION_FIELDS, sort: "created_at desc", limit: 20, titles: "true" }, signal)) : [];
  return { payments, deliveries, invoices: [...invoices, ...credits], refunds, exceptions, audit };
}

function Section({ id, title, children, action }: { id: string; title: string; children: ReactNode; action?: ReactNode }) {
  return (
    <section className="card" aria-labelledby={id}>
      <div className="card-head">
        <h2 id={id}>{title}</h2>
        <span className="spacer" />
        {action}
      </div>
      <div className="card-body">{children}</div>
    </section>
  );
}

const open = (entity: string, id: string) => `/open/${entity}/${id}`;

function Items({ o, code }: { o: Full; code?: string | null }) {
  const held = ["To Deliver and Bill", "To Deliver"].includes(o.status ?? "");
  const lines = o.items ?? [];
  const say = (v: unknown) => amountText(v, code);
  return (
    <table className="table cm-table">
      <thead>
        <tr>
          <th>{tr("Item")}</th>
          <th className="num">{tr("Qty")}</th>
          <th className="num">{tr("Price")}</th>
          <th className="num">{tr("Amount")}</th>
          <th>{tr("Sent")}</th>
        </tr>
      </thead>
      <tbody>
        {lines.map((l) => {
          const qty = Number(l.qty ?? 0);
          const sent = Number(l.delivered_qty ?? 0);
          return (
            <tr key={l.id}>
              <td data-label="Item">{l.item_name}</td>
              <td className="num" data-label="Qty">
                {units(qty)}
              </td>
              <td className="num" data-label="Price">
                {say(l.rate)}
              </td>
              <td className="num" data-label="Amount">
                {say(l.amount)}
              </td>
              <td data-label="Sent">
                {sent >= qty ? <Badge tone="green">{tr("Sent")}</Badge> : held ? <Badge tone="neutral" title={tr("This order holds these in the warehouse until they are sent")}>{sent > 0 ? `${units(sent)} of ${units(qty)} sent` : "In stock for this order"}</Badge> : <span className="faint">{sent > 0 ? `${units(sent)} of ${units(qty)}` : "Not sent"}</span>}
              </td>
            </tr>
          );
        })}
      </tbody>
      <tfoot>
        {o.total != null && o.total !== o.grand_total && (
          <tr>
            <td colSpan={3} data-label="">
              {tr("Items")}
            </td>
            <td className="num">{say(o.total)}</td>
            <td />
          </tr>
        )}
        {(o.taxes ?? [])
          .filter((t) => Number(t.tax_amount ?? 0) !== 0)
          .map((t, i) => (
            <tr key={i}>
              <td colSpan={3}>{t.description ?? "Tax"}</td>
              <td className="num">{say(t.deduct ? -Number(t.tax_amount) : t.tax_amount)}</td>
              <td />
            </tr>
          ))}
        <tr className="bold">
          <td colSpan={3}>{tr("Total")}</td>
          <td className="num">{say(o.rounded_total ?? o.grand_total)}</td>
          <td />
        </tr>
      </tfoot>
    </table>
  );
}

function Payment({ p, order, mayAsk, mayTake, onPaid, onRefund }: { p: Request; order: Full; mayAsk: boolean; mayTake: boolean; onPaid: () => void; onRefund: () => void }) {
  const sum = (v: unknown) => amountText(v, p.currency_code);
  const gateway = gatewayName(p.gateway);
  return (
    <li className="cm-payment" data-status={p.status}>
      <div className="cm-payment-head">
        <strong>{sum(p.amount)}</strong>
        <span>via {isOffline(p.gateway) ? p.gateway : gateway}</span>
        <Badge tone={statusTone(p.status)}>{p.status}</Badge>
      </div>
      <p className="faint cm-payment-line">
        {p.status === "Paid" && p.paid_at ? `Paid ${relativeTime(p.paid_at)}` : p.status === "Initiated" ? (p.expires_at ? `Open until ${relativeTime(p.expires_at)}` : "Open until it is paid") : p.status === "Expired" ? "Ran out of time" : p.status === "Failed" ? p.failure_reason ?? "The payment failed" : "Cancelled"}
        {p.gateway_reference && (
          <>
            {" · "}
            <code className="mono">{p.gateway_reference}</code>{" "}
            <button type="button" className="icon-btn icon-btn-sm" aria-label={tr("Copy payment reference")} title={tr("Copy payment reference")} onClick={() => void copy(p.gateway_reference ?? "", "Payment reference")}>
              <Icon name="copy" size={13} />
            </button>
          </>
        )}
      </p>
      {Number(p.refunded_amount ?? 0) > 0 && <p className="cm-payment-line">{sum(p.refunded_amount)} {tr("sent back so far.")}</p>}
      <div className="row">
        {p.status === "Initiated" && p.payment_url && (
          <Button size="sm" icon="copy" onClick={() => void copy(p.payment_url ?? "", "Payment link")}>
            {tr("Copy payment link")}
          </Button>
        )}
        {p.status === "Initiated" && isOffline(p.gateway) && mayTake && (
          <Button size="sm" variant="primary" icon="check" onClick={onPaid}>
            {tr("Mark paid")}
          </Button>
        )}
        {p.status === "Paid" && mayAsk && order.docstatus === 1 && (
          <Button size="sm" icon="refresh" onClick={onRefund}>
            {tr("Send money back…")}
          </Button>
        )}
      </div>
    </li>
  );
}

function OrderPage({ id }: { id: string }) {
  const qc = useQueryClient();
  const rights = useRights();
  const [shipping, setShipping] = useState(false);
  const [refunding, setRefunding] = useState(false);
  const [showAll, setShowAll] = useState(false);
  const [busy, setBusy] = useState(false);
  const order = useQuery({ queryKey: ["commerce", "order", id], queryFn: ({ signal }) => api.get<Full>(docPath("SalesOrder", id), { titles: true }, signal) });
  const more = useQuery({ queryKey: ["commerce", "order", id, "related"], enabled: !!order.data, queryFn: ({ signal }) => related(id, signal) });
  const refresh = useCallback(() => void qc.invalidateQueries({ queryKey: ["commerce"] }), [qc]);
  const mine = useCallback((e: { id: string }) => void (e.id === id && refresh()), [id, refresh]);
  useLiveEntity("SalesOrder", mine);
  useLiveEntity("PaymentRequest", refresh);
  useLiveEntity("OrderRefund", refresh);
  useLiveEntity("DeliveryNote", refresh);
  useLiveEntity("SalesInvoice", refresh);
  useLiveEntity("PaymentException", refresh);
  const o = order.data;
  useShellTitle(o?.number ? `Order ${o.number}` : null);

  if (order.error) return <ErrorState error={order.error} onRetry={() => void order.refetch()} />;
  if (!o) return <PageLoading label={tr("Loading the order")} />;
  if (!o.commerce_web_order) {
    return (
      <EmptyState title={tr("This is not a web order")} icon="file">
        <a href={open("SalesOrder", id)}>{tr("Open it in sales")}</a>
      </EmptyState>
    );
  }
  const cancelled = o.docstatus === 2 || o.status === "Cancelled";
  // An order placed before orders remembered how they are paid learns it from its latest payment request.
  const next = nextStep(withMethods([o], (more.data?.payments ?? []).map((p) => ({ sales_order: o.id, gateway: p.gateway })))[0]);
  const code = more.data?.payments[0]?.currency_code ?? o._titles?.currency;
  const say = (v: unknown, c?: string | null) => amountText(v, c ?? code);
  const paidRequest = more.data?.payments.find((p) => p.status === "Paid");
  const delivered = Number(o.per_delivered ?? 0) >= 100;
  const billed = Number(o.per_billed ?? 0) >= 100;
  const steps = [
    { id: "placed", label: "Placed", done: true },
    { id: "paid", label: o.commerce_payment_status === "Not Required" ? "Nothing to pay" : "Paid", done: isPaid(o) },
    { id: "sent", label: "Shipped", done: delivered },
    { id: "invoiced", label: "Invoiced", done: billed },
  ];
  const current = steps.findIndex((s) => !s.done);
  const done = async (job: () => Promise<boolean>) => {
    setBusy(true);
    try {
      if (await job()) refresh();
    } finally {
      setBusy(false);
    }
  };
  const primary =
    next.step === "ship" && rights.ships ? (
      <Button variant="primary" icon="check" onClick={() => setShipping(true)}>
        {tr("Ship")}
      </Button>
    ) : next.step === "invoice" && rights.ships ? (
      <Button variant="primary" icon="doc" loading={busy} onClick={() => void done(() => invoiceOrder(o))}>
        {next.label}
      </Button>
    ) : next.step === "mark_paid" && rights.takesPayment ? (
      <Button variant="primary" icon="check" loading={busy} onClick={() => void done(() => markPaid(o))}>
        {next.label}
      </Button>
    ) : null;
  const story = more.data
    ? timeline(
        {
          order: { created_at: o.created_at, posting_date: o.posting_date, customer_name: o.customer_name, rounded_total: o.rounded_total, currency_code: code },
          payments: more.data.payments,
          deliveries: more.data.deliveries,
          invoices: more.data.invoices,
          refunds: more.data.refunds,
          exceptions: more.data.exceptions,
          audit: more.data.audit,
        },
        { money: say, gateway: gatewayName },
      )
    : [];
  const { shown, hidden } = fold(story, showAll ? 1000 : 8);
  const counts = more.data ? { refunds: more.data.refunds.length, exceptions: more.data.exceptions.length } : { refunds: 0, exceptions: 0 };

  return (
    <div className="cm-page cm-order-page">
      <div className="crumbs">
        <Link to="/orders">{tr("Orders")}</Link>
        <Icon name="chevronRight" size={12} />
      </div>
      <header className="cm-order-top">
        <div className="cm-order-title">
          <h1 className="page-title">{o.number}</h1>
          <p className="page-sub">{[o.customer_name, o.posting_date ? formatDate(o.posting_date) : null, say(o.rounded_total)].filter(Boolean).join(" · ")}</p>
        </div>
        <div className="page-actions">
          {o.commerce_payment_status && <Badge tone={statusTone(o.commerce_payment_status)}>{o.commerce_payment_status}</Badge>}
          <Badge tone={statusTone(statusWords(o))}>{statusWords(o)}</Badge>
          {primary}
          {paidRequest && rights.asks && !cancelled && (
            <Button icon="refresh" onClick={() => setRefunding(true)}>
              {tr("Send money back…")}
            </Button>
          )}
          <Menu ariaLabel="More about this order" icon="chevronDown" label={tr("More")} buttonClass="btn btn-default btn-md">
            {(close) => (
              <>
                <MenuItem
                  onSelect={() => {
                    close();
                    void trackingLink(id)
                      .then((link) => (link ? copy(link, "Tracking link") : toast(tr("The store has no web address yet, so there is no tracking link to make."), "error")))
                      .catch(() => toast(tr("The tracking link could not be made."), "error"));
                  }}
                >
                  {tr("Copy the shopper's tracking link")}
                </MenuItem>
                <MenuItem
                  onSelect={() => {
                    close();
                    window.location.assign(open("SalesOrder", id));
                  }}
                >
                  {tr("Open in sales")}
                </MenuItem>
                <MenuItem
                  onSelect={() => {
                    close();
                    window.print();
                  }}
                >
                  {tr("Print this page")}
                </MenuItem>
              </>
            )}
          </Menu>
        </div>
      </header>

      {cancelled ? (
        <Alert tone="warning">{tr("This order was cancelled.")}</Alert>
      ) : (
        <>
          <ol className="cm-strip" aria-label={tr("Where the order is")}>
            {steps.map((s, i) => (
              <li key={s.id} data-state={s.done ? "past" : i === current ? "current" : undefined}>
                <span aria-current={i === current ? "step" : undefined}>{s.label}</span>
              </li>
            ))}
          </ol>
          <p className="cm-next">
            <strong>{next.step === "none" || next.step === "wait" ? "Now:" : "Next:"}</strong> {next.step === "ship" && !rights.ships ? "A sales user ships this order." : next.why}
          </p>
        </>
      )}

      <div className="cm-body">
        <div className="cm-main">
          <Section id="o-items" title={tr("Items")}>
            <Items o={o} code={code} />
          </Section>

          <Section id="o-pay" title={tr("Payment")}>
            {more.error ? (
              <ErrorState error={more.error} onRetry={() => void more.refetch()} />
            ) : !more.data ? (
              <p className="muted">{tr("Loading payments…")}</p>
            ) : more.data.payments.length ? (
              <ul className="cm-payments">
                {more.data.payments.map((p) => (
                  <Payment key={p.id} p={p} order={o} mayAsk={rights.asks} mayTake={rights.takesPayment} onPaid={() => void done(() => markPaid(o))} onRefund={() => setRefunding(true)} />
                ))}
              </ul>
            ) : (
              <p className="muted">{o.commerce_payment_status === "Not Required" ? "Nothing was due on this order." : "No payment was asked for, or you cannot see the payments."}</p>
            )}
          </Section>

          <Section id="o-send" title={tr("Shipping and invoices")}>
            {more.error ? (
              <p className="muted">{tr("Shipping and invoices could not be loaded.")}</p>
            ) : !more.data ? (
              <p className="muted">{tr("Loading…")}</p>
            ) : more.data.deliveries.length || more.data.invoices.length ? (
              <ul className="cm-docs">
                {more.data.deliveries.map((n) => (
                  <li key={n.id}>
                    <Icon name={n.is_return ? "left" : "upload"} size={15} />
                    <div>
                      <a href={open("DeliveryNote", n.id)}>{n.number}</a> <span className="faint">{n.is_return ? "goods came back" : "shipped"} {n.posting_date ? formatDate(n.posting_date) : ""}</span>
                      {(n.transporter || n.commerce_tracking_number) && (
                        <div className="faint">
                          {[n.transporter, n.commerce_tracking_number].filter(Boolean).join(" · ")}
                          {n.commerce_tracking_url && /^https:\/\//.test(n.commerce_tracking_url) && (
                            <>
                              {" "}
                              <a href={n.commerce_tracking_url} target="_blank" rel="noopener noreferrer">
                                {tr("Track it")}
                              </a>
                            </>
                          )}
                        </div>
                      )}
                    </div>
                  </li>
                ))}
                {more.data.invoices.map((v) => (
                  <li key={v.id}>
                    <Icon name="doc" size={15} />
                    <div>
                      <a href={open("SalesInvoice", v.id)}>{v.number}</a> <span className="faint">{v.is_return ? "credit note" : "invoice"} {v.posting_date ? formatDate(v.posting_date) : ""}</span>
                      <div>
                        {say(v.rounded_total)} {v.status && <Badge tone={statusTone(v.status)}>{v.status}</Badge>}
                      </div>
                    </div>
                  </li>
                ))}
              </ul>
            ) : (
              <p className="muted">{cancelled ? "Nothing shipped." : "Nothing has shipped yet."}</p>
            )}
          </Section>

          {(counts.refunds > 0 || counts.exceptions > 0) && more.data && (
            <Section id="o-refunds" title={tr("Money sent back")}>
              <ul className="cm-moneys">
                {more.data.refunds.map((r) => (
                  <RefundRow key={r.id} r={r} rights={rights} showOrder={false} onChange={refresh} />
                ))}
                {more.data.exceptions.map((x) => (
                  <ExceptionRow key={x.id} x={x} rights={rights} order={{ id, number: o.number }} onChange={refresh} />
                ))}
              </ul>
            </Section>
          )}
        </div>

        <aside className="cm-side" aria-label={tr("About this order")}>
          <Section id="o-customer" title={tr("Customer")}>
            <dl className="kv">
              <div>
                <dt>{tr("Name")}</dt>
                <dd>{o.customer_name}</dd>
              </div>
              {o.commerce_contact_email && (
                <div>
                  <dt>{tr("Email")}</dt>
                  <dd>
                    <a href={`mailto:${o.commerce_contact_email}`}>{o.commerce_contact_email}</a>
                  </dd>
                </div>
              )}
              {o.commerce_contact_phone && (
                <div>
                  <dt>{tr("Phone")}</dt>
                  <dd>
                    <a href={`tel:${o.commerce_contact_phone}`}>{o.commerce_contact_phone}</a>
                  </dd>
                </div>
              )}
              {o.commerce_payment_method && (
                <div>
                  <dt>{tr("Pays with")}</dt>
                  <dd>{o.commerce_payment_method}</dd>
                </div>
              )}
              {o._titles?.commerce_shipping_rule && (
                <div>
                  <dt>{tr("Delivery")}</dt>
                  <dd>{o._titles.commerce_shipping_rule}</dd>
                </div>
              )}
              {o._titles?.commerce_coupon && (
                <div>
                  <dt>{tr("Coupon")}</dt>
                  <dd>{o._titles.commerce_coupon}</dd>
                </div>
              )}
            </dl>
            {o.customer && (
              <p className="cm-tight">
                <a href={open("Customer", o.customer)}>{tr("Open the customer")}</a>
              </p>
            )}
          </Section>

          {o.commerce_shipping_address && (
            <Section id="o-ship" title={tr("Goes to")}>
              <address className="cm-address">{o.commerce_shipping_address}</address>
              {o.commerce_billing_address && o.commerce_billing_address !== o.commerce_shipping_address && (
                <>
                  <h3 className="cm-minor">{tr("Bill to")}</h3>
                  <address className="cm-address">{o.commerce_billing_address}</address>
                </>
              )}
            </Section>
          )}

          {o.commerce_notes && (
            <Section id="o-notes" title={tr("Customer's note")}>
              <p className="cm-note">{o.commerce_notes}</p>
            </Section>
          )}

          <Section id="o-story" title={tr("What happened")}>
            {more.error ? (
              <p className="muted">{tr("The story could not be loaded.")}</p>
            ) : !more.data ? (
              <p className="muted">{tr("Loading…")}</p>
            ) : (
              <>
                <ol className="cm-story">
                  {shown.map((e) => (
                    <li key={e.key} data-kind={e.kind}>
                      <span className="cm-story-icon" aria-hidden="true">
                        <Icon name={{ order: "file", payment: "check", delivery: "upload", invoice: "doc", refund: "refresh", exception: "alert", change: "edit" }[e.kind]} size={12} />
                      </span>
                      <div>
                        <div className="cm-story-line">
                          <strong>{e.title}</strong>
                          <time className="faint" dateTime={e.at} title={formatDateTime(e.at)}>
                            {relativeTime(e.at)}
                          </time>
                        </div>
                        {e.detail && <div className="faint cm-story-detail">{e.detail}</div>}
                        {e.who && <div className="faint cm-story-detail">{e.who}</div>}
                        {e.to && /^https:\/\//.test(e.to) && (
                          <a href={e.to} target="_blank" rel="noopener noreferrer">
                            {tr("Track it")}
                          </a>
                        )}
                      </div>
                    </li>
                  ))}
                </ol>
                {hidden > 0 && (
                  <button type="button" className="link-btn" onClick={() => setShowAll(true)}>
                    {tr("Show {hidden} older", { hidden })} {hidden === 1 ? "entry" : "entries"}
                  </button>
                )}
              </>
            )}
          </Section>
        </aside>
      </div>

      {shipping && (
        <ShipDialog
          order={o}
          onClose={() => setShipping(false)}
          onDone={() => {
            setShipping(false);
            refresh();
          }}
        />
      )}
      {refunding && (
        <RefundDialog
          order={o}
          rights={rights}
          onClose={() => setRefunding(false)}
          onDone={() => {
            setRefunding(false);
            refresh();
          }}
        />
      )}
    </div>
  );
}

export function OrderRoute() {
  const { id = "" } = useParams();
  return <OrderPage key={id} id={id} />;
}
