// Changing and cancelling a web order from the desk, kept apart so it is tested without a browser: what
// is asked of the server (`Update Web Order`'s changes and address, `Cancel Web Order`'s parameters),
// the difference worked out from what the desk can see before it is saved, and what the server will do
// with that difference (src/edit.rs, src/refund.rs). The server prices the change again (promotions, tax,
// delivery) and says what it came to; nothing here claims to be that figure.

import { t } from "@ferrum/client";
import { isOffline } from "./queues";

export interface OrderLine {
  id: string;
  item?: string | null;
  item_name?: string | null;
  qty?: string | null;
  rate?: string | null;
  amount?: string | null;
  delivered_qty?: string | null;
}

export interface PayRow {
  id: string;
  gateway?: string | null;
  amount?: string | null;
  status?: string | null;
  currency_code?: string | null;
  refunded_amount?: string | null;
}

export interface Address {
  line1: string;
  line2: string;
  city: string;
  state: string;
  postal_code: string;
  country: string;
}

export const emptyAddress = (): Address => ({ line1: "", line2: "", city: "", state: "", postal_code: "", country: "" });

export interface Added {
  key: number;
  item: string | null;
  title: string;
  qty: string;
}

/** What the desk has changed so far. A quantity is kept per order line id as typed; a line not in it is unchanged. */
export interface Draft {
  qty: Record<string, string>;
  added: Added[];
  address: Address | null;
}

export const emptyDraft = (): Draft => ({ qty: {}, added: [], address: null });

const num = (v: unknown) => {
  const n = Number(v ?? 0);
  return Number.isFinite(n) ? n : 0;
};

/** The gift card's label in the server's payment requests: no gateway has this name. */
export const GIFT_CARD = "Gift Card";

/** The line the store's delivery charge is on: the server prices it, the desk does not edit it. */
export const isCharge = (l: OrderLine, shippingItem: string | null | undefined) => !!shippingItem && l.item === shippingItem;

/** A typed quantity: a number of at least 0, or null when it is not one. */
export function quantity(raw: string): number | null {
  if (raw.trim() === "") return null;
  const n = Number(raw);
  return Number.isFinite(n) && n >= 0 ? Math.round(n * 1000) / 1000 : null;
}

/** Why a quantity cannot be saved, in words, or null: below what shipped is refused by ERP, so it is said here first. */
export function lineProblem(l: OrderLine, raw: string | undefined): string | null {
  if (raw === undefined) return null;
  const q = quantity(raw);
  if (q === null) return t("Use a number of 0 or more.");
  const sent = num(l.delivered_qty);
  if (q < sent) return t("{sent} of this line have shipped: it cannot go below that.", { sent });
  return null;
}

/** The changes the server is asked for: `{line, qty}` by the line's number on the order (1 is the first), `{item, qty}` for an item added. */
export function changes(lines: OrderLine[], d: Draft): ({ line: number; qty: string } | { item: string; qty: string })[] {
  const out: ({ line: number; qty: string } | { item: string; qty: string })[] = [];
  lines.forEach((l, i) => {
    const raw = d.qty[l.id];
    const q = raw === undefined ? null : quantity(raw);
    if (q !== null && q !== num(l.qty)) out.push({ line: i + 1, qty: String(q) });
  });
  for (const a of d.added) {
    const q = quantity(a.qty);
    if (a.item && q !== null && q > 0) out.push({ item: a.item, qty: String(q) });
  }
  return out;
}

/** The address as the server takes it: only when something is filled in; it needs a line, a city and a country. */
export function addressProblem(a: Address | null): string | null {
  if (!a) return null;
  if (!a.line1.trim() || !a.city.trim() || !a.country.trim()) return t("A new address needs a street, a city and a country.");
  if (!/^[A-Za-z]{2}$/.test(a.country.trim())) return t("Give the country as its two letters, such as IN or DE.");
  return null;
}

export function addressParam(a: Address | null) {
  if (!a) return undefined;
  const out: Record<string, string> = {};
  for (const [k, v] of Object.entries(a)) if (v.trim()) out[k] = k === "country" ? v.trim().toUpperCase() : v.trim();
  return JSON.stringify(out);
}

/** How the server settles what an order now comes to against what was paid (src/edit.rs, `settlement`). */
export type Settlement =
  | { kind: "none"; amount?: string }
  /** The shopper is asked: their open request is replaced (`replaces_open_request`), or a new one is made on the paid order. */
  | { kind: "charge"; amount: string; gateway?: string | null; label?: string | null; replaces_open_request?: boolean }
  /** Sent back through the payment that can take it, as an Adjustment refund. */
  | { kind: "refund"; amount: string; gateway?: string | null; label?: string | null; status?: string | null }
  /** Neither could be done: recorded as owed, by the shopper or by the store. */
  | { kind: "owed"; amount: string; by: "shopper" | "store" };

export interface PreviewLine {
  line: number;
  id?: string | null;
  item?: string | null;
  name?: string | null;
  qty: string;
  rate: string;
  amount: string;
  delivery: boolean;
  delivered_qty?: string;
}

/** What `Update Web Order` answers, and what the preview route answers before it: money as text (src/edit.rs). */
export interface Saved {
  total?: string;
  difference?: string;
  owed?: string;
  payment_request?: string | null;
  refund?: string | null;
  settlement?: Settlement;
  lines?: PreviewLine[];
  totals?: { goods: string; delivery: string; taxes: string; grand_total: string; rounded_total: string; was: string };
  preview?: boolean;
}

/** The body of the preview route: the save's own parameters and the order. The refund is never approved in a preview. */
export function previewBody(order: string, lines: OrderLine[], d: Draft, shippingRule: string) {
  return { sales_order: order, ...updateParams(lines, d, "", false, false), shipping_rule: shippingRule || undefined };
}

/** What the server will do with the difference, in words; `approvable` when the refund waits for the desk's choice. */
export function settlementWords(s: Settlement, money: (v: unknown) => string, via: (gateway?: string | null, label?: string | null) => string): { text: string; approvable: boolean } {
  switch (s.kind) {
    case "none":
      return { text: t("Nothing is asked for and nothing is sent back."), approvable: false };
    case "charge":
      return {
        text: s.replaces_open_request
          ? t("The shopper has not paid yet: their open payment request is replaced by one for {amount}, through {via}.", { amount: money(s.amount), via: via(s.gateway, s.label) })
          : t("The shopper is asked for {amount} through {via}, with a payment link on their order page.", { amount: money(s.amount), via: via(s.gateway, s.label) }),
        approvable: false,
      };
    case "refund":
      return isOffline(s.gateway)
        ? { text: t("{amount} is noted as a refund through {via}, which has no gateway: you hand it back and record that you did.", { amount: money(s.amount), via: via(s.gateway, s.label) }), approvable: false }
        : { text: t("{amount} is sent back through {via} as an adjustment refund.", { amount: money(s.amount), via: via(s.gateway, s.label) }), approvable: true };
    default:
      return {
        text: s.by === "store"
          ? t("No payment can take a refund of this size, so it is recorded as owed to the shopper: the store owes {amount}.", { amount: money(s.amount) })
          : t("No payment can be asked for it, so it is recorded as owed by the shopper: {amount} is still to collect.", { amount: money(s.amount) }),
        approvable: false,
      };
  }
}

/** What the desk is told once it is saved, with the figures the server worked out. */
export function savedWords(r: Saved, money: (v: unknown) => string): string {
  const diff = num(r.difference);
  const parts = [t("Saved. The order now comes to {total}.", { total: money(r.total) })];
  if (diff > 0) parts.push(t("That is {amount} more than before.", { amount: money(diff) }));
  if (diff < 0) parts.push(t("That is {amount} less than before.", { amount: money(-diff) }));
  if (r.payment_request) parts.push(t("The shopper is asked for it with a payment link on their order page."));
  else if (r.refund) parts.push(t("A refund of the difference was made."));
  if (num(r.owed) > 0) parts.push(t("The shopper still owes {amount}.", { amount: money(r.owed) }));
  if (num(r.owed) < 0) parts.push(t("The store owes the shopper {amount}.", { amount: money(-num(r.owed)) }));
  return parts.join(" ");
}

export interface Part {
  id: string;
  gateway: string;
  card: boolean;
  amount: number;
  offline: boolean;
}

/** What a cancellation sends back, per payment: the gift card's part first (it goes back onto the card), then each provider's. Nothing paid: nothing. */
export function parts(payments: PayRow[]): Part[] {
  const paid = payments.filter((p) => p.status === "Paid");
  const row = (p: PayRow): Part => ({ id: p.id, gateway: p.gateway ?? "", card: p.gateway === GIFT_CARD, amount: num(p.amount), offline: isOffline(p.gateway) });
  return [...paid.filter((p) => p.gateway === GIFT_CARD), ...paid.filter((p) => p.gateway !== GIFT_CARD)].map(row);
}

/** What happens to one part, in words, for the choice of sending it now or leaving it for a person. */
export function partWords(p: Part, approveNow: boolean, label: string): string {
  if (p.card) return approveNow ? t("goes back onto the gift card now, in the same step") : t("goes back onto the gift card when a person approves it");
  if (p.offline) return t("is handed back by you: {label} has no gateway, and you record that you did", { label });
  return approveNow ? t("goes back through {label}: the gateway carries it out, and if it fails the refund stays for you to try again", { label }) : t("waits for a person to approve it, then goes back through {label}", { label });
}

/** The parameters of `Cancel Web Order`. */
export function cancelParams(reason: string, note: string, approveNow: boolean) {
  return { reason: reason || undefined, note: note.trim() || undefined, approve: approveNow ? "Yes" : "No" };
}

/** The parameters of `Update Web Order`. */
export function updateParams(lines: OrderLine[], d: Draft, note: string, notify: boolean, approveRefund: boolean) {
  const asked = changes(lines, d);
  return {
    changes: asked.length ? JSON.stringify(asked) : undefined,
    address: addressParam(d.address),
    note: note.trim() || undefined,
    notify: notify ? "Yes" : "No",
    approve: approveRefund ? "Yes" : "No",
  };
}

/** Whether there is anything to save, and if not, what is missing. */
export function readiness(lines: OrderLine[], d: Draft, charge: (l: OrderLine) => boolean): { ok: boolean; why: string } {
  for (const l of lines) {
    if (charge(l)) continue;
    const p = lineProblem(l, d.qty[l.id]);
    if (p) return { ok: false, why: p };
  }
  for (const a of d.added) {
    if (!a.item) return { ok: false, why: t("Choose the item to add, or take the row away.") };
    const q = quantity(a.qty);
    if (q === null || q <= 0) return { ok: false, why: t("Add at least one of an item.") };
  }
  const a = addressProblem(d.address);
  if (a) return { ok: false, why: a };
  if (!changes(lines, d).length && !d.address) return { ok: false, why: t("Change a quantity, add an item or give a new address.") };
  return { ok: true, why: "" };
}
