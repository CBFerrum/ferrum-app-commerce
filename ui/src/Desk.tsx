// The web-order desk: the orders as they come in, sorted into what waits for money, what is to be
// picked and sent, and what is sent but not invoiced, longest-waiting first, each with the one thing
// to do next. A pick list turns the orders to send into what to take off the shelves. The queue and
// the search are in the address, so a link shows what you saw.

import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useCallback, useMemo, useState } from "react";
import { Link, useSearchParams } from "react-router";
import { api, docPath, t } from "@ferrum/client";
import { Badge, Button, EmptyState, ErrorState, Icon, Spinner, statusTone, useLiveEntity, useShellTitle } from "@ferrum/kit";
import { amountText, counts, nextStep, type Order, type PickOrder, pickList, queue, STAGES, type Stage, stage, statusWords, units, waitedDays } from "./queues";
import { f, invoiceOrder, list, markPaid, ORDER_FIELDS, useRights, WEB, workingOrders } from "./data";
import { ShipDialog } from "./Ship";
import { Tabs } from "./Tabs";

const currency = (o: Order) => o._titles?.currency;

/** "today", "yesterday", "3 days ago": how long the order has waited. */
function waitText(o: Order): string {
  const days = waitedDays(o);
  return days === 0 ? "today" : days === 1 ? "yesterday" : `${days} days ago`;
}

function Row({ o, onShip, onPaid, onInvoiced, busy }: { o: Order; onShip: (o: Order) => void; onPaid: (o: Order) => void; onInvoiced: (o: Order) => void; busy: boolean }) {
  const rights = useRights();
  const next = nextStep(o);
  const late = waitedDays(o) >= 2 && ["pay", "ship"].includes(stage(o));
  const act =
    next.step === "ship" ? (
      rights.ships ? (
        <Button variant="primary" size="sm" icon="check" onClick={() => onShip(o)}>
          {t("Ship")}
        </Button>
      ) : null
    ) : next.step === "invoice" ? (
      rights.ships ? (
        <Button variant="primary" size="sm" icon="doc" loading={busy} onClick={() => onInvoiced(o)}>
          {next.label}
        </Button>
      ) : null
    ) : next.step === "mark_paid" ? (
      rights.takesPayment ? (
        <Button variant="primary" size="sm" icon="check" loading={busy} onClick={() => onPaid(o)}>
          {next.label}
        </Button>
      ) : null
    ) : null;
  const cannot =
    !rights.ready ? null : (next.step === "ship" || next.step === "invoice") && !rights.ships ? "A sales user ships and invoices this." : next.step === "mark_paid" && !rights.takesPayment ? "Someone who takes payments marks this paid." : null;
  return (
    <li className="cm-order">
      <div className="cm-order-main">
        <div className="cm-order-line">
          <Link className="cm-order-no" to={`/o/${o.id}`}>
            {o.number}
          </Link>
          <span className="cm-order-who">{o.customer_name}</span>
        </div>
        <div className="cm-order-why">{cannot ?? next.why}</div>
        <div className="cm-order-badges">
          <span className={late ? "cm-late" : "faint"}>{late ? `Waiting since ${waitText(o)}` : `Placed ${waitText(o)}`}</span>
          {o.commerce_payment_status && <Badge tone={statusTone(o.commerce_payment_status)}>{o.commerce_payment_status}</Badge>}
          {stage(o) !== "done" && <Badge tone="neutral">{statusWords(o)}</Badge>}
        </div>
      </div>
      <div className="cm-order-side">
        <strong className="cm-order-total">{amountText(o.rounded_total, currency(o))}</strong>
        {act}
      </div>
    </li>
  );
}

const NOTHING: Record<Stage, [string, string]> = {
  pay: ["Nothing is waiting for money", "Every order is paid, or paid on delivery."],
  ship: ["Nothing to ship", "You are all caught up. New paid orders show up here."],
  invoice: ["Nothing to invoice", "Everything that was sent has its invoice."],
  done: ["No finished orders yet", "Delivered, settled and cancelled orders show up here."],
};

/** The shelves, for the orders to send: what to take, how many, for which orders. */
function PickList({ orders }: { orders: Order[] }) {
  const ids = orders.slice(0, 50).map((o) => o.id);
  const q = useQuery({
    queryKey: ["commerce", "pick", ids.join(",")],
    enabled: ids.length > 0,
    queryFn: async ({ signal }) => {
      const [docs, settings] = await Promise.all([
        Promise.all(ids.map((id) => api.get<PickOrder>(docPath("SalesOrder", id), { titles: true }, signal))),
        list<{ shipping_item?: string | null }>("StoreSettings", { fields: "shipping_item", limit: 1 }, signal).then((r) => r.rows[0]?.shipping_item ?? null).catch(() => null),
      ]);
      return { docs, shipping: settings };
    },
  });
  if (!ids.length) return <EmptyState title={t("Nothing to pick")} icon="check">{t("No orders are waiting to be sent.")}</EmptyState>;
  if (q.error) return <ErrorState error={q.error} onRetry={() => void q.refetch()} />;
  if (!q.data) return <Spinner label={t("Making the pick list")} />;
  const picks = pickList(q.data.docs, (l) => !q.data.shipping || l.item !== q.data.shipping);
  return (
    <section className="cm-pick" aria-label={t("Pick list")}>
      <div className="cm-pick-head">
        <p className="muted">
          {picks.length} {picks.length === 1 ? "item" : "items"} for {orders.length} {orders.length === 1 ? "order" : "orders"}
          {orders.length > ids.length ? ` (the first ${ids.length})` : ""}.
        </p>
        <Button icon="printer" onClick={() => window.print()}>
          {t("Print")}
        </Button>
      </div>
      {picks.length ? (
        <table className="table cm-pick-table">
          <thead>
            <tr>
              <th>{t("Item")}</th>
              <th>{t("Shelf")}</th>
              <th className="num">{t("Take")}</th>
              <th>{t("For")}</th>
            </tr>
          </thead>
          <tbody>
            {picks.map((p) => (
              <tr key={p.key}>
                <td data-label="Item">{p.item}</td>
                <td data-label="Shelf">{p.warehouse || "—"}</td>
                <td className="num" data-label="Take">
                  <strong>{units(p.qty)}</strong>
                </td>
                <td data-label="For">{p.orders.join(", ")}</td>
              </tr>
            ))}
          </tbody>
        </table>
      ) : (
        <p className="muted">{t("Everything on these orders has been taken already.")}</p>
      )}
    </section>
  );
}

/** Orders that are over, a page at a time, found by the server. */
function Finished({ search, onShip, onPaid, onInvoiced, busy }: { search: string; onShip: (o: Order) => void; onPaid: (o: Order) => void; onInvoiced: (o: Order) => void; busy: string | null }) {
  const [limit, setLimit] = useState(30);
  const q = useQuery({
    queryKey: ["commerce", "finished", search, limit],
    queryFn: ({ signal }) =>
      list<Order>(
        "SalesOrder",
        { fields: ORDER_FIELDS, titles: "true", sort: "posting_date desc", limit, count: "true", search: search.trim(), filters: f([WEB, ["status", "in", ["Completed", "Closed", "Cancelled"]]]) },
        signal,
      ),
  });
  if (q.error) return <ErrorState error={q.error} onRetry={() => void q.refetch()} />;
  if (!q.data) return <Spinner label={t("Loading finished orders")} />;
  if (!q.data.rows.length) return <EmptyState title={search ? "No finished order matches" : NOTHING.done[0]} icon="file">{search ? "Try another number or name." : NOTHING.done[1]}</EmptyState>;
  return (
    <>
      <ul className="cm-orders">
        {q.data.rows.map((o) => (
          <Row key={o.id} o={o} onShip={onShip} onPaid={onPaid} onInvoiced={onInvoiced} busy={busy === o.id} />
        ))}
      </ul>
      {(q.data.total ?? 0) > q.data.rows.length && (
        <p className="cm-more">
          <Button onClick={() => setLimit(limit + 30)} loading={q.isFetching}>
            {t("Show 30 more")}
          </Button>
        </p>
      )}
    </>
  );
}

export function Desk() {
  useShellTitle("Orders");
  const qc = useQueryClient();
  const [sp, setSp] = useSearchParams();
  const which = (STAGES.find((s) => s.id === sp.get("stage"))?.id ?? "ship") as Stage;
  const view = sp.get("view") === "pick" ? "pick" : "queue";
  const search = sp.get("q") ?? "";
  const [shipping, setShipping] = useState<Order | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const q = useQuery({ queryKey: ["commerce", "desk"], queryFn: ({ signal }) => workingOrders(signal) });
  const refresh = useCallback(() => void qc.invalidateQueries({ queryKey: ["commerce"] }), [qc]);
  useLiveEntity("SalesOrder", refresh);
  useLiveEntity("PaymentRequest", refresh);
  useLiveEntity("DeliveryNote", refresh);
  useLiveEntity("SalesInvoice", refresh);
  const set = (change: Record<string, string | null>) => {
    const next = new URLSearchParams(sp);
    for (const [k, v] of Object.entries(change)) (v ? next.set(k, v) : next.delete(k));
    setSp(next, { replace: true });
  };

  const orders = q.data ?? [];
  const tally = useMemo(() => counts(orders), [orders]);
  const shown = useMemo(() => queue(orders, which, search), [orders, which, search]);
  const run = async (o: Order, job: (o: Order) => Promise<boolean>) => {
    setBusy(o.id);
    try {
      if (await job(o)) refresh();
    } finally {
      setBusy(null);
    }
  };
  const row = { onShip: setShipping, onPaid: (o: Order) => void run(o, markPaid), onInvoiced: (o: Order) => void run(o, invoiceOrder) };

  const body = q.error ? (
    <ErrorState error={q.error} onRetry={() => void q.refetch()} />
  ) : !q.data ? (
    <Spinner label={t("Loading orders")} />
  ) : view === "pick" ? (
    <PickList orders={queue(orders, "ship")} />
  ) : which === "done" ? (
    <Finished search={search} {...row} busy={busy} />
  ) : shown.length ? (
    <ul className="cm-orders">
      {shown.map((o) => (
        <Row key={o.id} o={o} {...row} busy={busy === o.id} />
      ))}
    </ul>
  ) : search ? (
    <EmptyState title={t("No order matches")} icon="search">
      {t("Nothing in this queue matches “{search}”.", { search })}{" "}
      <button type="button" className="link-btn" onClick={() => set({ q: null })}>
        {t("Clear the search")}
      </button>
    </EmptyState>
  ) : (
    <EmptyState title={NOTHING[which][0]} icon="check">
      {NOTHING[which][1]}
    </EmptyState>
  );

  return (
    <div className="cm-page">
      <header className="cm-head">
        <div>
          <Tabs />
          <p className="page-sub" aria-live="polite">
            {q.data ? `${tally.pay + tally.ship + tally.invoice} open ${tally.pay + tally.ship + tally.invoice === 1 ? "order" : "orders"}${q.data.length >= 200 ? " (the oldest 200)" : ""}` : " "}
          </p>
        </div>
        <div className="cm-tools">
          <div className="segmented" role="group" aria-label={t("View")}>
            <button type="button" aria-pressed={view === "queue"} onClick={() => set({ view: null })}>
              {t("Queue")}
            </button>
            <button type="button" aria-pressed={view === "pick"} onClick={() => set({ view: "pick" })}>
              <Icon name="list" size={13} /> {t("Pick list")}
            </button>
          </div>
          {view === "queue" && <input className="input cm-search" type="search" placeholder={t("Find an order…")} aria-label={t("Find an order by number, customer or email")} value={search} onChange={(e) => set({ q: e.target.value || null })} />}
        </div>
      </header>
      {view === "queue" && (
        <div className="cm-stages" role="tablist" aria-label={t("Queues")}>
          {STAGES.map((s) => (
            <button key={s.id} type="button" role="tab" aria-selected={which === s.id} title={s.hint} onClick={() => set({ stage: s.id === "ship" ? null : s.id })}>
              {s.label}
              {s.id !== "done" && q.data && <span className="cm-stage-n">{tally[s.id]}</span>}
            </button>
          ))}
        </div>
      )}
      {body}
      {shipping && (
        <ShipDialog
          order={shipping}
          onClose={() => setShipping(null)}
          onDone={() => {
            setShipping(null);
            refresh();
          }}
        />
      )}
    </div>
  );
}
