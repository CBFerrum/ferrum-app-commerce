// Commerce's home, the store's day at a glance: what needs a decision, web sales today and over the
// last week and month, the orders at the desk (what waits for money, to be sent, to be invoiced),
// payments that did not go through, reviews waiting for moderation (published or rejected in place)
// and the catalog's state. Orders open on their own page here; the store's records in forms and
// lists of the records frame.

import { useQuery, useQueryClient } from "@tanstack/react-query";
import { type ReactNode, useCallback, useMemo, useState } from "react";
import { Link } from "react-router";
import { api, errorMessage, todayIso, t as tr, isolate } from "@ferrum/client";
import { Alert, Badge, Button, ListSkeleton, StatePanel, statusTone, toast, useLiveEntity, useShellTitle, placed } from "@ferrum/kit";
import { can, useMetaIndex } from "@ferrum/records";
import { amountText, counts, queue, STAGES, waitedDays } from "./queues";
import { REQUEST_PAYMENT } from "./tones";
import { f, list, soft, useWaiting, workingOrders } from "./data";
import { Tabs } from "./Tabs";
import { type DayRow, daysBefore, money, totals } from "./store";

const today = () => todayIso();

function Sales() {
  const end = today();
  const q = useQuery({
    queryKey: ["commerce", "sales", end],
    queryFn: ({ signal }) => api.get<{ rows: DayRow[] }>("/api/reports/web_sales", { from_date: daysBefore(end, 29), to_date: end, group_by: "Day" }, signal).then((r) => r.rows),
  });
  if (q.error) return <p className="muted">{tr("Sales cannot be shown ({error}).", { error: errorMessage(q.error) })}</p>;
  if (!q.data) return <ListSkeleton rows={1} />;
  const tiles = [
    { label: tr("Today"), t: totals(q.data, end, end) },
    { label: tr("Last 7 days"), t: totals(q.data, daysBefore(end, 6), end) },
    { label: tr("Last 30 days"), t: totals(q.data, daysBefore(end, 29), end) },
  ];
  return (
    <div className="cm-tiles">
      {tiles.map(({ label, t }) => (
        <Link key={label} className="cm-tile" to={`/r/web_sales?from_date=${label === "Today" ? end : daysBefore(end, label === "Last 7 days" ? 6 : 29)}&to_date=${end}`}>
          <span className="faint">{isolate(label)}</span>
          <strong>{isolate(money(t.revenue))}</strong>
          <span>
            {t.orders} {t.orders === 1 ? tr("order") : tr("orders")}
          </span>
        </Link>
      ))}
    </div>
  );
}

function AtTheDesk() {
  const q = useQuery({ queryKey: ["commerce", "desk"], queryFn: ({ signal }) => workingOrders(signal) });
  const tally = useMemo(() => counts(q.data ?? []), [q.data]);
  if (q.error) return <p className="muted">{tr("Orders cannot be shown ({error}).", { error: errorMessage(q.error) })}</p>;
  if (!q.data) return <ListSkeleton rows={3} />;
  const oldest = queue(q.data, "ship").slice(0, 5);
  return (
    <>
      <div className="cm-tiles cm-tiles-small">
        {STAGES.filter((s) => s.id !== "done").map((s) => (
          <Link key={s.id} className="cm-tile" to={s.id === "ship" ? "/orders" : `/orders?stage=${s.id}`} title={s.hint}>
            <span className="faint">{isolate(s.label)}</span>
            <strong>{tally[s.id as "pay" | "ship" | "invoice"]}</strong>
          </Link>
        ))}
      </div>
      {oldest.length ? (
        <>
          <h3 className="cm-minor">{tr("Waiting longest to be sent")}</h3>
          <ul className="cm-rows" data-ferrum-nocard>
            {oldest.map((o) => (
              <li key={o.id}>
                <Link to={`/o/${o.id}`}>{isolate(o.number ?? "")}</Link> <span className="faint">{isolate([o.customer_name, waitedDays(o) ? tr("{o} days", { o: waitedDays(o) }) : "today"].filter(Boolean).join(" · "))}</span>
                <span className="cm-end">
                  <strong>{isolate(amountText(o.rounded_total, o._titles?.currency))}</strong>
                </span>
              </li>
            ))}
          </ul>
        </>
      ) : (
        <p className="muted">{tally.pay + tally.invoice ? tr("Nothing is ready to send.") : tr("Every web order is sent and settled.")}</p>
      )}
      <p className="cm-tight">
        <Link to="/orders">{tr("Go to the orders")}</Link>
      </p>
    </>
  );
}

interface Payment {
  id: string;
  sales_order?: string | null;
  order_number?: string | null;
  payer_name?: string | null;
  gateway?: string | null;
  amount?: string | null;
  currency_code?: string | null;
  status?: string | null;
}

function Payments() {
  const q = useQuery({
    queryKey: ["commerce", "payments"],
    queryFn: ({ signal }) => soft(list<Payment>("PaymentRequest", { filters: f([["status", "in", ["Failed", "Expired"]]]), sort: "created_at desc", limit: 10, fields: "sales_order,order_number,payer_name,gateway,amount,currency_code,status" }, signal)),
  });
  if (q.error) return <p className="muted">{tr("Payments cannot be shown ({error}).", { error: errorMessage(q.error) })}</p>;
  if (!q.data) return <ListSkeleton rows={2} />;
  if (!q.data.length) return <p className="muted">{tr("No failed or expired payments.")}</p>;
  return (
    <ul className="cm-rows" data-ferrum-nocard>
      {q.data.map((p) => (
        <li key={p.id}>
          {p.sales_order ? <Link to={`/o/${p.sales_order}`}>{p.order_number ?? tr("Payment")}</Link> : <span>{p.order_number ?? tr("Payment")}</span>} <span className="faint">{isolate([p.payer_name, p.gateway].filter(Boolean).join(" · "))}</span>
          <span className="cm-end">
            <Badge tone={statusTone(p.status, REQUEST_PAYMENT)}>{isolate(p.status ?? "")}</Badge>
            <strong>{isolate(amountText(p.amount, p.currency_code))}</strong>
          </span>
        </li>
      ))}
    </ul>
  );
}

interface Review {
  id: string;
  version?: number;
  rating?: number | null;
  title?: string | null;
  review?: string | null;
  reviewer_name?: string | null;
  _titles?: Record<string, string>;
}

function Reviews() {
  const qc = useQueryClient();
  const [busy, setBusy] = useState<string | null>(null);
  const q = useQuery({
    queryKey: ["commerce", "reviews"],
    queryFn: ({ signal }) => list<Review>("ItemReview", { filters: f([["status", "=", "Pending"]]), sort: "created_at asc", limit: 20, count: "true", titles: "true", fields: "website_item,rating,title,review,reviewer_name,version" }, signal),
  });
  const decide = async (r: Review, status: "Approved" | "Rejected") => {
    setBusy(r.id);
    try {
      await api.patch(`/api/v1/ItemReview/${r.id}`, { status }, { ifMatch: r.version });
      toast((status === "Approved" ? tr("Published: {reviewer_name}'s review", { reviewer_name: String(r.reviewer_name) }) : tr("Rejected: {reviewer_name}'s review", { reviewer_name: String(r.reviewer_name) })));
      await qc.invalidateQueries({ queryKey: ["commerce", "reviews"] });
    } catch (err) {
      toast(errorMessage(err), "error");
    } finally {
      setBusy(null);
    }
  };
  if (q.error) return <p className="muted">{tr("Reviews cannot be shown ({error}).", { error: errorMessage(q.error) })}</p>;
  if (!q.data) return <ListSkeleton rows={2} />;
  if (!q.data.rows.length) return <p className="muted">{tr("No reviews waiting.")}</p>;
  return (
    <ul className="cm-reviews">
      {q.data.rows.map((r) => (
        <li key={r.id}>
          <div>
            <span aria-label={tr("{rating} of 5", { rating: String(r.rating) })}>{isolate("★".repeat(r.rating ?? 0))}{isolate("☆".repeat(5 - (r.rating ?? 0)))}</span> <strong>{isolate(r.title ?? "")}</strong>{isolate(" ")}
            <span className="faint">
              {tr("{reviewer_name} on", { reviewer_name: r.reviewer_name ?? "" })} {r._titles?.website_item ?? tr("an item")}
            </span>
          </div>
          {r.review && <p className="cm-review-text">{isolate(r.review)}</p>}
          <div className="row">
            <Button size="sm" variant="primary" loading={busy === r.id} onClick={() => void decide(r, "Approved")}>
              {tr("Publish")}
            </Button>
            <Button size="sm" disabled={busy === r.id} onClick={() => void decide(r, "Rejected")}>
              {tr("Reject")}
            </Button>
          </div>
        </li>
      ))}
    </ul>
  );
}

function Catalog() {
  const count = (...filters: unknown[]) => list("WebsiteItem", { filters: f(filters), limit: 1, count: "true", fields: "title" }).then((r) => r.total ?? 0);
  const q = useQuery({ queryKey: ["commerce", "catalog"], queryFn: () => Promise.all([count(["published", "=", true], ["variant_of", "is", "not set"]), count(["published", "=", false], ["variant_of", "is", "not set"]), count(["variant_of", "is", "set"])]) });
  if (q.error) return <p className="muted">{tr("The catalog cannot be shown ({error}).", { error: errorMessage(q.error) })}</p>;
  if (!q.data) return <ListSkeleton rows={2} />;
  const [shown, hidden, variants] = q.data;
  const open = (...filters: unknown[]) => `/e/WebsiteItem?filters=${encodeURIComponent(f(filters))}`;
  return (
    <>
      <p>
        <Link to={open(["published", "=", true], ["variant_of", "is", "not set"])}>{tr("{shown} published", { shown })}</Link>,{isolate(" ")}
        <Link to={open(["published", "=", false], ["variant_of", "is", "not set"])}>{tr("{hidden} not published", { hidden })}</Link>
        {variants > 0 && (
          <>
            {placed(tr(", and {p1} of them"), { p1: <Link to={open(["variant_of", "is", "set"])}>{variants} {variants === 1 ? tr("variant") : tr("variants")}</Link> })}
          </>
        )}
        . <Link to="/e/WebsiteItem/new">{tr("Add an item to the store")}</Link>
      </p>
      <p className="faint cm-tight">{tr("A size or a colour is an item of its own: on its form, say which item it is a variant of.")}</p>
    </>
  );
}

export function CommerceHome() {
  useShellTitle(tr("Store"));
  const meta = useMetaIndex().data;
  const readable = (e: string) => !!meta?.entities.find((x) => x.name === e && can(x.permissions, "read"));
  const qc = useQueryClient();
  const waiting = useWaiting();
  const refresh = useCallback(() => void qc.invalidateQueries({ queryKey: ["commerce"] }), [qc]);
  useLiveEntity("SalesOrder", refresh);
  useLiveEntity("ItemReview", refresh);
  useLiveEntity("PaymentRequest", refresh);
  useLiveEntity("OrderRefund", refresh);
  useLiveEntity("PaymentException", refresh);
  if (meta && !readable("WebsiteItem") && !readable("SalesOrder")) {
    return (
      <StatePanel kind="denied">{tr("Ask an administrator for the Website Manager role.")}</StatePanel>
    );
  }
  const card = (id: string, title: string, body: ReactNode) => (
    <section className="card" aria-labelledby={id}>
      <div className="card-head">
        <h2 id={id}>{isolate(title)}</h2>
      </div>
      <div className="card-body">{body}</div>
    </section>
  );
  return (
    <div className="cm-page">
      <header className="cm-head">
        <div>
          <Tabs />
          <p className="page-sub">{tr("Web sales, the orders at the desk, payments and reviews.")}</p>
        </div>
      </header>
      {!!waiting && (
        <Alert
          tone="warning"
          action={
            <Link className="btn btn-default btn-sm" to="/money">
              {tr("Decide")}
            </Link>
          }
        >
          {waiting} {(waiting === 1 ? tr("refund or payment needs a decision.") : tr("refunds and payments need a decision."))}
        </Alert>
      )}
      {readable("SalesOrder") && <Sales />}
      <div className="cm-columns">
        {readable("SalesOrder") && card("cm-deliver", tr("Orders at the desk"), <AtTheDesk />)}
        {readable("ItemReview") && card("cm-reviews", tr("Reviews waiting"), <Reviews />)}
        {readable("PaymentRequest") && card("cm-payments", tr("Payments that did not go through"), <Payments />)}
        {readable("WebsiteItem") && card("cm-catalog", tr("Catalog"), <Catalog />)}
      </div>
    </div>
  );
}
