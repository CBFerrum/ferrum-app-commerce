// The returns desk's rules, kept apart so they are tested without a browser: where a request waits, what its
// terms come to in words, whether it was asked within the store's window, and what the refund will come to
// before the desk confirms it. The server decides (src/shopper.rs): it holds the windows, makes the refund
// and says no with its reason; this only reads what it kept and says it.

import { t } from "@ferrum/client";

export interface ReturnLine {
  id?: string;
  sales_order_line: string;
  item?: string | null;
  item_name?: string | null;
  qty: string;
}

export interface ReturnRequest {
  id: string;
  number?: string | null;
  order_number?: string | null;
  sales_order?: string | null;
  email?: string | null;
  status?: string | null;
  reason?: string | null;
  shopper_note?: string | null;
  resolution?: string | null;
  shipping_paid_by?: string | null;
  restocking_fee_percent?: string | number | null;
  decision_note?: string | null;
  decided_by?: string | null;
  refund?: string | null;
  requested_at?: string | null;
  created_at?: string | null;
  lines?: ReturnLine[];
  _titles?: Record<string, string>;
}

/** Where a request waits: for the desk's decision, for the goods to arrive, or finished. */
export type Stage = "decide" | "receive" | "done";

export const STAGES: { id: Stage; label: string }[] = [
  { id: "decide", get label() { return t("To decide"); } },
  { id: "receive", get label() { return t("Waiting for the goods"); } },
  { id: "done", get label() { return t("Done"); } },
];

export const stage = (r: Pick<ReturnRequest, "status">): Stage => (r.status === "Requested" ? "decide" : r.status === "Approved" ? "receive" : "done");

/** ReturnRequest.status, by its stored id. */
export const RETURN_TONES = { Requested: "amber", Approved: "blue", Declined: "neutral", Received: "green", Refunded: "green" } as const;

const num = (v: unknown) => {
  const n = Number(v ?? 0);
  return Number.isFinite(n) ? n : 0;
};

/** Where the refund goes, as the store's terms said when it was asked for. */
export const destinationWords = (resolution?: string | null) => (resolution === "Store Credit" ? t("As store credit: a new code is sent to the shopper") : t("Back to the payment it came from"));

export const shippingWords = (paidBy?: string | null) => (paidBy === "Store" ? t("The store pays to have it sent back") : t("The shopper pays to send it back"));

export const feeWords = (percent: unknown) => (num(percent) > 0 ? t("{percent}% restocking fee is kept from what the goods credit", { percent: num(percent) }) : t("No restocking fee"));

/** Whole days between two dates (a delivery and the day it was asked for), or null when either is missing. */
export function daysBetween(from?: string | null, to?: string | null): number | null {
  if (!from || !to) return null;
  const a = Date.parse(from.slice(0, 10));
  const b = Date.parse(to.slice(0, 10));
  return Number.isFinite(a) && Number.isFinite(b) ? Math.round((b - a) / 86_400_000) : null;
}

/** When it was asked against the store's window: the words, and whether it was inside it. A window of 0 is a store that takes none. */
export function windowWords(delivered: string | null | undefined, asked: string | null | undefined, windowDays: number | null | undefined): { words: string; inside: boolean | null } {
  const days = daysBetween(delivered, asked);
  if (days === null) return { words: windowDays ? t("The store takes returns for {days} days after delivery.", { days: windowDays }) : "", inside: null };
  if (!windowDays) return { words: t("Asked {days} days after delivery. The store takes no returns now: this was asked under an earlier window.", { days }), inside: null };
  const inside = days <= windowDays;
  return { words: inside ? t("Asked {days} days after delivery, inside the store's {window} days.", { days, window: windowDays }) : t("Asked {days} days after delivery, outside the store's {window} days.", { days, window: windowDays }), inside };
}

/** What the desk is about to refund: the goods' credit, less the restocking fee, and so what goes back. All approximate until the server makes the credit note. */
export function refundSum(credit: number, percent: unknown): { credit: number; fee: number; net: number } {
  const fee = Math.round(credit * Math.max(0, Math.min(100, num(percent))) ) / 100;
  return { credit, fee, net: Math.round((credit - fee) * 100) / 100 };
}

/** The lines as the refund plan asks for them: the order line, how many. */
export const planLines = (lines: ReturnLine[]) => lines.map((l) => ({ sales_order_line: l.sales_order_line, qty: l.qty, restock: true }));

/** Stock alerts of one item: how many shoppers wait, and how many were told. */
export interface Alert {
  id: string;
  website_item?: string | null;
  item_title?: string | null;
  item_slug?: string | null;
  email?: string | null;
  status?: string | null;
  sent_at?: string | null;
  created_at?: string | null;
}

export interface Group {
  key: string;
  title: string;
  slug: string | null;
  item: string | null;
  waiting: Alert[];
  sent: Alert[];
  oldest: string | null;
}

/** The alerts grouped by the item they wait for, the one with most waiting first. */
export function groups(alerts: Alert[]): Group[] {
  const by = new Map<string, Group>();
  for (const a of alerts) {
    const key = a.website_item ?? a.item_slug ?? a.id;
    const g = by.get(key) ?? { key, title: a.item_title ?? a.item_slug ?? "", slug: a.item_slug ?? null, item: a.website_item ?? null, waiting: [], sent: [], oldest: null };
    (a.status === "Sent" ? g.sent : g.waiting).push(a);
    if (a.status !== "Sent" && a.created_at && (!g.oldest || a.created_at < g.oldest)) g.oldest = a.created_at;
    by.set(key, g);
  }
  return [...by.values()].sort((x, y) => y.waiting.length - x.waiting.length || x.title.localeCompare(y.title));
}
