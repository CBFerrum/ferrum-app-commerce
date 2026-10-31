// Commerce's home, the store's admin: web sales today and over the last week and month, the web
// orders still to deliver, payments that did not go through, reviews waiting for moderation
// (approved or rejected in place) and the catalog's state. Orders open in ERP (/open), where they
// are delivered and billed; the store's own records in its forms and lists here.

import { useQuery, useQueryClient } from "@tanstack/react-query";
import { type ReactNode, useCallback, useState } from "react";
import { Link } from "react-router";
import { api, errorMessage } from "@ferrum/client";
import { Badge, Button, EmptyState, Spinner, statusTone, toast, useLiveEntity, useShellTitle } from "@ferrum/kit";
import { can, useMetaIndex } from "@ferrum/records";
import { type DayRow, daysBefore, money, totals } from "./store";

const list = <T,>(entity: string, query: Record<string, string | number>, signal?: AbortSignal) =>
  api.get<{ rows: T[]; total?: number }>(`/api/v1/${entity}`, query, signal);
const f = (filters: unknown[]) => JSON.stringify(filters);
const today = () => new Date().toISOString().slice(0, 10);

function Sales() {
  const end = today();
  const q = useQuery({
    queryKey: ["commerce", "sales", end],
    queryFn: ({ signal }) => api.get<{ rows: DayRow[] }>("/api/reports/web_sales", { from_date: daysBefore(end, 29), to_date: end, group_by: "Day" }, signal).then((r) => r.rows),
  });
  if (q.error) return <p className="muted">Sales cannot be shown ({errorMessage(q.error)}).</p>;
  if (!q.data) return <Spinner label="Loading sales" />;
  const tiles = [
    { label: "Today", t: totals(q.data, end, end) },
    { label: "Last 7 days", t: totals(q.data, daysBefore(end, 6), end) },
    { label: "Last 30 days", t: totals(q.data, daysBefore(end, 29), end) },
  ];
  return (
    <div className="cm-tiles">
      {tiles.map(({ label, t }) => (
        <Link key={label} className="cm-tile" to={`r/web_sales?from_date=${label === "Today" ? end : daysBefore(end, label === "Last 7 days" ? 6 : 29)}&to_date=${end}`}>
          <span className="faint">{label}</span>
          <strong>{money(t.revenue)}</strong>
          <span>
            {t.orders} {t.orders === 1 ? "order" : "orders"}
          </span>
        </Link>
      ))}
    </div>
  );
}

interface Order {
  id: string;
  number?: string | null;
  customer_name?: string | null;
  posting_date?: string | null;
  grand_total?: string | null;
  status?: string | null;
  commerce_payment_status?: string | null;
}

function ToDeliver() {
  const q = useQuery({
    queryKey: ["commerce", "to-deliver"],
    queryFn: ({ signal }) =>
      list<Order>(
        "SalesOrder",
        { filters: f([["commerce_web_order", "=", true], ["docstatus", "=", 1], ["status", "in", ["To Deliver and Bill", "To Deliver"]]]), sort: "posting_date asc", limit: 20, count: "true", fields: "number,customer_name,posting_date,grand_total,status,commerce_payment_status" },
        signal,
      ),
  });
  if (q.error) return <p className="muted">Orders cannot be shown ({errorMessage(q.error)}).</p>;
  if (!q.data) return <Spinner small label="Loading orders" />;
  if (!q.data.rows.length) return <p className="muted">Every web order is delivered.</p>;
  return (
    <>
      <ul className="cm-rows" data-ferrum-nocard>
        {q.data.rows.map((o) => (
          <li key={o.id}>
            {/* Orders are ERP's: delivered and billed there. */}
            <a href={`/open/SalesOrder/${o.id}`}>{o.number}</a> <span className="faint">{[o.customer_name, o.posting_date].filter(Boolean).join(" · ")}</span>
            <span className="cm-end">
              {o.commerce_payment_status && <Badge tone={statusTone(o.commerce_payment_status)}>{o.commerce_payment_status}</Badge>}
              <strong>{money(Number(o.grand_total ?? 0))}</strong>
            </span>
          </li>
        ))}
      </ul>
      {(q.data.total ?? 0) > q.data.rows.length && <p className="muted">The {q.data.rows.length} oldest of {q.data.total}.</p>}
    </>
  );
}

interface Payment {
  id: string;
  order_number?: string | null;
  payer_name?: string | null;
  gateway?: string | null;
  amount?: string | null;
  status?: string | null;
}

function Payments() {
  const q = useQuery({
    queryKey: ["commerce", "payments"],
    queryFn: ({ signal }) =>
      list<Payment>("PaymentRequest", { filters: f([["status", "in", ["Failed", "Expired"]]]), sort: "created_at desc", limit: 10, fields: "order_number,payer_name,gateway,amount,status" }, signal).then((r) => r.rows),
  });
  if (q.error) return null;
  if (!q.data) return <Spinner small label="Loading payments" />;
  if (!q.data.length) return <p className="muted">No failed or expired payments.</p>;
  return (
    <ul className="cm-rows" data-ferrum-nocard>
      {q.data.map((p) => (
        <li key={p.id}>
          <Link to={`e/PaymentRequest/${p.id}`}>{p.order_number ?? "Payment"}</Link> <span className="faint">{[p.payer_name, p.gateway].filter(Boolean).join(" · ")}</span>
          <span className="cm-end">
            <Badge tone={statusTone(p.status)}>{p.status}</Badge>
            <strong>{money(Number(p.amount ?? 0))}</strong>
          </span>
        </li>
      ))}
    </ul>
  );
}

interface Review {
  id: string;
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
    queryFn: ({ signal }) =>
      list<Review>("ItemReview", { filters: f([["status", "=", "Pending"]]), sort: "created_at asc", limit: 20, count: "true", titles: "true", fields: "website_item,rating,title,review,reviewer_name" }, signal),
  });
  const decide = async (r: Review, status: "Approved" | "Rejected") => {
    setBusy(r.id);
    try {
      await api.patch(`/api/v1/ItemReview/${r.id}`, { status });
      toast(`${status === "Approved" ? "Published" : "Rejected"}: ${r.reviewer_name}'s review`);
      await qc.invalidateQueries({ queryKey: ["commerce", "reviews"] });
    } catch (err) {
      toast(errorMessage(err), "error");
    } finally {
      setBusy(null);
    }
  };
  if (q.error) return null;
  if (!q.data) return <Spinner small label="Loading reviews" />;
  if (!q.data.rows.length) return <p className="muted">No reviews waiting.</p>;
  return (
    <ul className="cm-reviews">
      {q.data.rows.map((r) => (
        <li key={r.id}>
          <div>
            <span aria-label={`${r.rating} of 5`}>{"★".repeat(r.rating ?? 0)}{"☆".repeat(5 - (r.rating ?? 0))}</span> <strong>{r.title}</strong>{" "}
            <span className="faint">
              {r.reviewer_name} on {r._titles?.website_item ?? "an item"}
            </span>
          </div>
          {r.review && <p className="cm-review-text">{r.review}</p>}
          <div className="row">
            <Button size="sm" variant="primary" loading={busy === r.id} onClick={() => void decide(r, "Approved")}>
              Publish
            </Button>
            <Button size="sm" disabled={busy === r.id} onClick={() => void decide(r, "Rejected")}>
              Reject
            </Button>
          </div>
        </li>
      ))}
    </ul>
  );
}

function Catalog() {
  const count = (published: boolean) =>
    list("WebsiteItem", { filters: f([["published", "=", published]]), limit: 1, count: "true", fields: "title" }).then((r) => r.total ?? 0);
  const q = useQuery({ queryKey: ["commerce", "catalog"], queryFn: () => Promise.all([count(true), count(false)]) });
  if (!q.data) return <Spinner small label="Loading the catalog" />;
  const [shown, hidden] = q.data;
  return (
    <p>
      <Link to={`e/WebsiteItem?filters=${encodeURIComponent(f([["published", "=", true]]))}`}>{shown} published</Link>,{" "}
      <Link to={`e/WebsiteItem?filters=${encodeURIComponent(f([["published", "=", false]]))}`}>{hidden} not published</Link>. <Link to="e/WebsiteItem/new">Add an item to the store</Link>
    </p>
  );
}

export function CommerceHome() {
  useShellTitle("Store");
  const meta = useMetaIndex().data;
  const readable = (e: string) => !!meta?.entities.find((x) => x.name === e && can(x.permissions, "read"));
  const qc = useQueryClient();
  const refresh = useCallback(() => void qc.invalidateQueries({ queryKey: ["commerce"] }), [qc]);
  useLiveEntity("SalesOrder", refresh);
  useLiveEntity("ItemReview", refresh);
  useLiveEntity("PaymentRequest", refresh);
  if (meta && !readable("WebsiteItem") && !readable("SalesOrder")) {
    return (
      <EmptyState title="Nothing here for you" icon="store">
        <p className="muted">Ask an administrator for the Website Manager role.</p>
      </EmptyState>
    );
  }
  const card = (id: string, title: string, body: ReactNode) => (
    <section className="card" aria-labelledby={id}>
      <div className="card-head">
        <h2 id={id}>{title}</h2>
      </div>
      <div className="card-body">{body}</div>
    </section>
  );
  return (
    <div className="stack">
      <div className="page-head">
        <div>
          <h1 className="page-title">Store</h1>
          <p className="page-sub">Web sales, orders to deliver, payments and reviews.</p>
        </div>
      </div>
      {readable("SalesOrder") && <Sales />}
      <div className="cm-columns">
        {readable("SalesOrder") && card("cm-deliver", "Web orders to deliver", <ToDeliver />)}
        {readable("ItemReview") && card("cm-reviews", "Reviews waiting", <Reviews />)}
        {readable("PaymentRequest") && card("cm-payments", "Payments that did not go through", <Payments />)}
        {readable("WebsiteItem") && card("cm-catalog", "Catalog", <Catalog />)}
      </div>
    </div>
  );
}
