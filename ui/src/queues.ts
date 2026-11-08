import { t as tr } from "@ferrum/client";
// The web-order desk's rules, kept apart so they are tested without a browser: which queue an order
// sits in, the one next step it asks for, how the queues count, how long it has waited, and what has
// to be picked from the shelves. The server holds the real rules (what may be shipped, invoiced or
// paid); this only reads the statuses it keeps and says them in words.

export interface Order {
  id: string;
  number?: string | null;
  customer_name?: string | null;
  customer?: string | null;
  posting_date?: string | null;
  rounded_total?: string | number | null;
  grand_total?: string | number | null;
  status?: string | null;
  docstatus?: number | null;
  per_delivered?: string | number | null;
  per_billed?: string | number | null;
  commerce_web_order?: boolean | null;
  commerce_payment_status?: string | null;
  commerce_payment_method?: string | null;
  commerce_contact_email?: string | null;
  commerce_shipping_address?: string | null;
  _titles?: Record<string, string>;
}

/** The desk's queues, in the order an order moves through them. */
export type Stage = "pay" | "ship" | "invoice" | "done";

export const STAGES: { id: Stage; label: string; hint: string }[] = [
  { id: "pay", get label() { return tr("To pay"); }, get hint() { return tr("Waiting for money"); } },
  { id: "ship", get label() { return tr("To ship"); }, get hint() { return tr("Paid, or paid on delivery: pick, pack and send"); } },
  { id: "invoice", get label() { return tr("To invoice"); }, get hint() { return tr("Sent, not invoiced yet"); } },
  { id: "done", get label() { return tr("Done"); }, get hint() { return tr("Delivered and settled, or cancelled"); } },
];

const num = (v: unknown) => {
  const n = Number(v ?? 0);
  return Number.isFinite(n) ? n : 0;
};

/** The shop takes payment on delivery, or by hand: no gateway confirms it. */
export const isOffline = (method?: string | null) => method === "Manual" || method === "Cash on Delivery";

/** Money has come in for this order (or none was due). */
export const isPaid = (o: Pick<Order, "commerce_payment_status">) => ["Paid", "Partly Refunded", "Refunded", "Not Required"].includes(o.commerce_payment_status ?? "");

const isOver = (o: Order) => o.docstatus === 2 || ["Completed", "Closed", "Cancelled"].includes(o.status ?? "");

/** Which queue an order is in. */
export function stage(o: Order): Stage {
  if (isOver(o)) return "done";
  const delivered = num(o.per_delivered) >= 100;
  const billed = num(o.per_billed) >= 100;
  if (!delivered) return isPaid(o) || o.commerce_payment_method === "Cash on Delivery" ? "ship" : "pay";
  if (!billed) return "invoice";
  // Everything is sent and invoiced; the money has not come in (cash on delivery, a bank transfer).
  return isPaid(o) ? "done" : "pay";
}

export type Step = "ship" | "invoice" | "mark_paid" | "wait" | "none";

export interface Next {
  step: Step;
  /** The button, when there is one. */
  label: string;
  /** Why, in a sentence a person would say. */
  why: string;
}

/** The one thing to do next, or why there is nothing to do. */
export function nextStep(o: Order): Next {
  const method = o.commerce_payment_method;
  switch (stage(o)) {
    case "ship":
      return {
        step: "ship",
        label: tr("Ship"),
        why: method === "Cash on Delivery" && !isPaid(o) ? tr("Cash on delivery: the courier collects the money.") : o.commerce_payment_status === "Not Required" ? tr("Nothing to pay. Pack it and send it.") : tr("Paid. Pack it and hand it to the carrier."),
      };
    case "invoice":
      return { step: "invoice", label: tr("Create invoice"), why: tr("Sent, but not invoiced yet.") };
    case "pay":
      if (o.commerce_payment_status === "Failed" || o.commerce_payment_status === "Expired") {
        return { step: "wait", label: "", why: (o.commerce_payment_status === "Failed" ? tr("The payment failed. The shopper can pay again from their order link.") : tr("The payment ran out of time. The shopper can pay again from their order link.")) };
      }
      if (method === "Manual") return { step: "mark_paid", label: tr("Mark paid"), why: tr("Bank transfer: mark it paid when the money arrives.") };
      if (method === "Cash on Delivery") return { step: "mark_paid", label: tr("Mark paid"), why: tr("Sent. Record the cash the courier brought.") };
      return { step: "wait", label: "", why: method ? tr("Waiting for {method} to confirm the shopper's payment.", { method }) : tr("Waiting for the shopper to pay.") };
    default:
      return { step: "none", label: "", why: o.status === "Cancelled" || o.docstatus === 2 ? tr("Cancelled.") : tr("Nothing left to do.") };
  }
}

/** Orders placed before an order remembered how it is paid get the method from their latest payment
 *  request (`requests` newest first), so that a cash-on-delivery order is not mistaken for one waiting on a gateway. */
export function withMethods(orders: Order[], requests: { sales_order?: string | null; gateway?: string | null }[]): Order[] {
  const by = new Map<string, string>();
  for (const r of requests) if (r.sales_order && r.gateway && !by.has(r.sales_order)) by.set(r.sales_order, r.gateway);
  return orders.map((o) => (o.commerce_payment_method ? o : { ...o, commerce_payment_method: by.get(o.id) ?? null }));
}

/** Orders per queue. */
export function counts(orders: Order[]): Record<Stage, number> {
  const out: Record<Stage, number> = { pay: 0, ship: 0, invoice: 0, done: 0 };
  for (const o of orders) out[stage(o)] += 1;
  return out;
}

/** The orders of a queue (all of them for `all`), oldest first: the ones that waited longest lead. */
export function queue(orders: Order[], which: Stage | "all", search = ""): Order[] {
  const q = search.trim().toLowerCase();
  return orders
    .filter((o) => (which === "all" || stage(o) === which) && (!q || [o.number, o.customer_name, o.commerce_contact_email].some((v) => v?.toLowerCase().includes(q))))
    .sort((a, b) => (a.posting_date ?? "").localeCompare(b.posting_date ?? "") || (a.number ?? "").localeCompare(b.number ?? ""));
}

/** How long ago, in the units a person says: "just now", "3 h", "2 days". */
export function ago(at: string | null | undefined, now: Date = new Date()): string {
  if (!at) return "";
  const t = new Date(at.length <= 10 ? `${at}T00:00:00` : at).getTime();
  if (!Number.isFinite(t)) return "";
  const mins = Math.floor((now.getTime() - t) / 60_000);
  if (mins < 1) return tr("just now");
  if (mins < 60) return tr("{mins} min", { mins });
  const hours = Math.floor(mins / 60);
  if (hours < 24) return `${hours} h`;
  const days = Math.floor(hours / 24);
  return days === 1 ? "1 day" : tr("{days} days", { days });
}

/** Whole days an order has waited, for flagging the ones that have waited too long. */
export function waitedDays(o: Order, now: Date = new Date()): number {
  if (!o.posting_date) return 0;
  const t = new Date(`${o.posting_date}T00:00:00`).getTime();
  return Number.isFinite(t) ? Math.max(0, Math.floor((now.getTime() - t) / 86_400_000)) : 0;
}

/** An order status a shopper's shop assistant would say. */
export function statusWords(o: Order): string {
  if (o.docstatus === 2 || o.status === "Cancelled") return "Cancelled";
  switch (o.status) {
    case "To Deliver and Bill":
    case "To Deliver":
      return tr("To deliver");
    case "To Bill":
      return tr("To invoice");
    default:
      return o.status ?? "";
  }
}

export interface PickLine {
  item?: string | null;
  item_name?: string | null;
  qty?: string | number | null;
  delivered_qty?: string | number | null;
  warehouse?: string | null;
  _titles?: Record<string, string>;
}

export interface PickOrder {
  id: string;
  number?: string | null;
  items?: PickLine[];
  _titles?: Record<string, string>;
}

export interface ToPick {
  key: string;
  item: string;
  warehouse: string;
  qty: number;
  orders: string[];
}

/** What to take off the shelves for these orders: each item once per warehouse, with the quantity
 *  still to send and the orders it is for. The delivery charge is not on a shelf. */
export function pickList(orders: PickOrder[], isGoods: (line: PickLine) => boolean = () => true): ToPick[] {
  const by = new Map<string, ToPick>();
  for (const o of orders) {
    for (const l of o.items ?? []) {
      const open = num(l.qty) - num(l.delivered_qty);
      if (open <= 0 || !isGoods(l)) continue;
      const warehouse = l._titles?.warehouse ?? o._titles?.[`warehouse:${l.warehouse}`] ?? "";
      const item = l.item_name || l._titles?.item || "Item";
      const key = `${l.item ?? item}|${l.warehouse ?? ""}`;
      const row = by.get(key) ?? { key, item, warehouse, qty: 0, orders: [] };
      row.qty += open;
      if (o.number && !row.orders.includes(o.number)) row.orders.push(o.number);
      by.set(key, row);
    }
  }
  return [...by.values()].sort((a, b) => a.warehouse.localeCompare(b.warehouse) || a.item.localeCompare(b.item));
}

/** A number of units as people write it: 2, 0.5, never 2.000. */
export const units = (n: number) => String(Math.round(n * 1000) / 1000);

/** An amount with its currency, in the viewer's own way of writing money. */
export function amountText(value: unknown, code?: string | null, locale?: string): string {
  const n = Number(value ?? 0);
  if (!Number.isFinite(n)) return "";
  if (code && /^[A-Za-z]{3}$/.test(code)) {
    try {
      return new Intl.NumberFormat(locale, { style: "currency", currency: code.toUpperCase() }).format(n);
    } catch {
      // An unknown code: written out below.
    }
  }
  return `${code ? `${code} ` : ""}${n.toLocaleString(locale, { minimumFractionDigits: 2, maximumFractionDigits: 2 })}`;
}
