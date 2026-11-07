import { t as tr } from "@ferrum/client";
// Money that needs a person: refunds waiting for a decision, and payments a gateway took and could not
// book. Where each stands, in words, and which list it belongs on. Pure, so it is tested without a
// browser. The server keeps the states and what may follow each; this only says them.

export type Tone = "neutral" | "blue" | "green" | "amber" | "red" | "purple";

export interface Refund {
  id: string;
  version?: number;
  number?: string | null;
  order_number?: string | null;
  sales_order?: string | null;
  kind?: string | null;
  amount?: string | number | null;
  currency_code?: string | null;
  gateway?: string | null;
  status?: string | null;
  requested_by?: string | null;
  approved_by?: string | null;
  approved_at?: string | null;
  refunded_at?: string | null;
  refund_reference?: string | null;
  refund_error?: string | null;
  booked?: boolean | null;
  booking_error?: string | null;
  decision_note?: string | null;
  note?: string | null;
  payment_reference?: string | null;
  created_at?: string | null;
  _titles?: Record<string, string>;
}

export interface Exception {
  id: string;
  version?: number;
  gateway?: string | null;
  gateway_reference?: string | null;
  payment_request?: string | null;
  amount?: string | number | null;
  currency_code?: string | null;
  reason?: string | null;
  status?: string | null;
  approved_by?: string | null;
  approved_at?: string | null;
  refund_reference?: string | null;
  refund_error?: string | null;
  note?: string | null;
  created_at?: string | null;
  _titles?: Record<string, string>;
}

/** Where a thing waits: for a person's decision, for the gateway, or finished. */
export type Bucket = "decide" | "waiting" | "done";

export const BUCKETS: { id: Bucket; label: string }[] = [
  { id: "decide", get label() { return tr("Needs a decision"); } },
  { id: "waiting", get label() { return tr("With the gateway"); } },
  { id: "done", get label() { return tr("Done"); } },
];

export function refundBucket(r: Refund): Bucket {
  switch (r.status) {
    case "Requested":
    case "Refund Failed":
      return "decide";
    case "Approved":
      return "waiting";
    // The money went; the books did not follow: a person puts that right.
    case "Refunded":
      return r.booked ? "done" : "decide";
    default:
      return "done";
  }
}

export function exceptionBucket(x: Exception): Bucket {
  switch (x.status) {
    case "Open":
    case "Refund Failed":
      return "decide";
    case "Refund Approved":
      return "waiting";
    default:
      return "done";
  }
}

/** A gateway sends an approved refund within minutes; past this, a person should look. */
export const STUCK_MINUTES = 15;

export function stuck(approvedAt: string | null | undefined, now: Date = new Date()): boolean {
  if (!approvedAt) return false;
  const t = new Date(approvedAt).getTime();
  return Number.isFinite(t) && now.getTime() - t > STUCK_MINUTES * 60_000;
}

export interface Words {
  headline: string;
  detail: string;
  tone: Tone;
}

const KINDS: Record<string, string> = { Return: "goods coming back", Cancellation: "order cancelled before it shipped" };

export const gatewayName = (g?: string | null) => (g ? g.charAt(0).toUpperCase() + g.slice(1) : "the gateway");

/** The refund's state as a person would say it. `money` writes an amount. */
export function refundWords(r: Refund, money: (v: unknown, code?: string | null) => string, now: Date = new Date()): Words {
  const who = r.requested_by ?? "Someone";
  const gateway = gatewayName(r.gateway);
  const offline = r.gateway === "Manual" || r.gateway === "Cash on Delivery";
  switch (r.status) {
    case "Requested":
      return { headline: "Waiting for a decision", detail: `${who} asked to send ${money(r.amount, r.currency_code)} back (${KINDS[r.kind ?? ""] ?? "refund"}).`, tone: "amber" };
    case "Approved":
      if (offline) return { headline: "Approved: waiting for you", detail: `${r.gateway} has no gateway to send it. Refund it yourself, then say you have.`, tone: "amber" };
      return stuck(r.approved_at, now)
        ? { headline: `Approved: ${gateway} has not sent it`, detail: `It should take a few minutes. If you refunded it from ${gateway}'s own dashboard, say so; otherwise ask whoever set up the gateway.`, tone: "amber" }
        : { headline: `Approved: ${gateway} is sending it`, detail: `${money(r.amount, r.currency_code)} goes back to the shopper.`, tone: "blue" };
    case "Refund Failed":
      return { headline: `${gateway} could not send it`, detail: r.refund_error ?? "The gateway refused.", tone: "red" };
    case "Refunded":
      return r.booked
        ? { headline: "Sent back", detail: `${money(r.amount, r.currency_code)} went back${r.refund_reference ? ` (${r.refund_reference})` : ""}.`, tone: "green" }
        : { headline: "Sent back, but not in the books yet", detail: r.booking_error ?? "Posting it to the books failed.", tone: "red" };
    case "Declined":
      return { headline: "Declined", detail: r.decision_note ?? "", tone: "neutral" };
    default:
      return { headline: r.status ?? "", detail: "", tone: "neutral" };
  }
}

/** The same for money a gateway took and could not book. */
export function exceptionWords(x: Exception, money: (v: unknown, code?: string | null) => string, now: Date = new Date()): Words {
  const gateway = gatewayName(x.gateway);
  const amount = x.amount != null && x.amount !== "" ? money(x.amount, x.currency_code) : "Money";
  switch (x.status) {
    case "Open":
      return { headline: "Taken, but could not be booked", detail: `${amount} reached ${gateway}. ${x.reason ?? ""}`.trim(), tone: "red" };
    case "Refund Approved":
      return stuck(x.approved_at, now)
        ? { headline: `Approved: ${gateway} has not sent it`, detail: `It should take a few minutes. If you refunded it yourself, say so.`, tone: "amber" }
        : { headline: `Approved: ${gateway} is sending it`, detail: `${amount} goes back to the payer.`, tone: "blue" };
    case "Refund Failed":
      return { headline: `${gateway} could not send the refund`, detail: x.refund_error ?? "The gateway refused.", tone: "red" };
    case "Refunded":
      return { headline: "Sent back", detail: `${amount} went back${x.refund_reference ? ` (${x.refund_reference})` : ""}.`, tone: "green" };
    case "Resolved":
      return { headline: "Settled another way", detail: x.note ?? "", tone: "neutral" };
    default:
      return { headline: x.status ?? "", detail: "", tone: "neutral" };
  }
}

/** What is waiting for a person, for the badge on the tab. */
export function waiting(refunds: Refund[], exceptions: Exception[]): number {
  return refunds.filter((r) => refundBucket(r) === "decide").length + exceptions.filter((x) => exceptionBucket(x) === "decide").length;
}
