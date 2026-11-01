// One order's story: when it was placed, what was asked of the shopper's payment and what came of it,
// what shipped and was invoiced, what was sent back, and what the audit trail says about the order
// itself, newest first, from the records that hold each. Old entries fold. Pure, so it is tested
// without a browser.

export interface Entry {
  key: string;
  at: string;
  kind: "order" | "payment" | "delivery" | "invoice" | "refund" | "exception" | "change";
  title: string;
  detail?: string | null;
  who?: string | null;
  /** A page of this app, or an outside address, the entry points to. */
  to?: string | null;
}

export interface Payment {
  id: string;
  gateway?: string | null;
  amount?: string | number | null;
  currency_code?: string | null;
  status?: string | null;
  created_at?: string | null;
  updated_at?: string | null;
  paid_at?: string | null;
  expires_at?: string | null;
  failure_reason?: string | null;
  gateway_reference?: string | null;
}

export interface Delivery {
  id: string;
  number?: string | null;
  posting_date?: string | null;
  created_at?: string | null;
  is_return?: boolean | null;
  transporter?: string | null;
  commerce_tracking_number?: string | null;
  commerce_tracking_url?: string | null;
}

export interface Invoice {
  id: string;
  number?: string | null;
  posting_date?: string | null;
  created_at?: string | null;
  is_return?: boolean | null;
  rounded_total?: string | number | null;
  status?: string | null;
}

export interface Refund {
  id: string;
  number?: string | null;
  kind?: string | null;
  amount?: string | number | null;
  currency_code?: string | null;
  status?: string | null;
  requested_by?: string | null;
  approved_by?: string | null;
  approved_at?: string | null;
  refunded_at?: string | null;
  refund_error?: string | null;
  decision_note?: string | null;
  created_at?: string | null;
  updated_at?: string | null;
}

export interface Exception {
  id: string;
  amount?: string | number | null;
  currency_code?: string | null;
  gateway?: string | null;
  reason?: string | null;
  created_at?: string | null;
}

export interface AuditEvent {
  id?: number;
  at: string;
  actor?: string | null;
  actor_name?: string | null;
  action: string;
}

export interface Input {
  order: { created_at?: string | null; posting_date?: string | null; customer_name?: string | null; rounded_total?: string | number | null; currency_code?: string | null };
  payments: Payment[];
  deliveries: Delivery[];
  invoices: Invoice[];
  refunds: Refund[];
  exceptions: Exception[];
  audit: AuditEvent[];
}

export interface Say {
  money: (v: unknown, code?: string | null) => string;
  gateway: (g?: string | null) => string;
}

/** A date alone sorts at midday, so it falls among that day's timed entries. */
const when = (at?: string | null, fallback?: string | null) => {
  const t = at || fallback || "";
  return t.length === 10 ? `${t}T12:00:00.000Z` : t;
};

export function timeline(i: Input, say: Say): Entry[] {
  const out: Entry[] = [];
  const add = (e: Entry) => e.at && out.push(e);
  add({ key: "order", at: when(i.order.created_at, i.order.posting_date), kind: "order", title: "Order placed", detail: [i.order.customer_name, i.order.rounded_total != null ? say.money(i.order.rounded_total, i.order.currency_code) : null].filter(Boolean).join(" · ") });
  for (const p of i.payments) {
    const how = say.gateway(p.gateway);
    add({ key: `p-${p.id}-ask`, at: when(p.created_at), kind: "payment", title: `Payment of ${say.money(p.amount, p.currency_code)} asked via ${how}` });
    if (p.paid_at) add({ key: `p-${p.id}-paid`, at: when(p.paid_at), kind: "payment", title: `Paid via ${how}`, detail: p.gateway_reference });
    else if (p.status === "Failed") add({ key: `p-${p.id}-end`, at: when(p.updated_at, p.created_at), kind: "payment", title: "Payment failed", detail: p.failure_reason });
    else if (p.status === "Expired") add({ key: `p-${p.id}-end`, at: when(p.expires_at, p.updated_at), kind: "payment", title: "Payment ran out of time" });
    else if (p.status === "Cancelled") add({ key: `p-${p.id}-end`, at: when(p.updated_at, p.created_at), kind: "payment", title: "Payment request cancelled" });
  }
  for (const n of i.deliveries) {
    add(
      n.is_return
        ? { key: `n-${n.id}`, at: when(n.created_at, n.posting_date), kind: "delivery", title: "Goods back in stock", detail: n.number }
        : { key: `n-${n.id}`, at: when(n.created_at, n.posting_date), kind: "delivery", title: "Shipped", detail: [n.transporter, n.commerce_tracking_number].filter(Boolean).join(" · ") || n.number, to: n.commerce_tracking_url },
    );
  }
  for (const v of i.invoices) {
    add({ key: `i-${v.id}`, at: when(v.created_at, v.posting_date), kind: "invoice", title: `${v.is_return ? "Credit note" : "Invoice"} ${v.number ?? ""}`.trim(), detail: v.rounded_total != null ? say.money(v.rounded_total) : null });
  }
  for (const r of i.refunds) {
    const sum = say.money(r.amount, r.currency_code);
    add({ key: `r-${r.id}-ask`, at: when(r.created_at), kind: "refund", title: `Refund of ${sum} asked for${r.kind === "Cancellation" ? " (order cancelled)" : ""}`, who: r.requested_by });
    if (r.approved_at) add({ key: `r-${r.id}-ok`, at: when(r.approved_at), kind: "refund", title: "Refund approved", who: r.approved_by });
    if (r.refunded_at) add({ key: `r-${r.id}-sent`, at: when(r.refunded_at), kind: "refund", title: `${sum} sent back` });
    else if (r.status === "Refund Failed") add({ key: `r-${r.id}-end`, at: when(r.updated_at, r.approved_at), kind: "refund", title: "The gateway could not send the refund", detail: r.refund_error });
    else if (r.status === "Declined") add({ key: `r-${r.id}-end`, at: when(r.updated_at, r.created_at), kind: "refund", title: "Refund declined", detail: r.decision_note });
  }
  for (const x of i.exceptions) {
    add({ key: `x-${x.id}`, at: when(x.created_at), kind: "exception", title: `${say.money(x.amount, x.currency_code)} reached ${say.gateway(x.gateway)} and could not be booked`, detail: x.reason });
  }
  for (const e of i.audit) {
    if (e.action === "cancel") add({ key: `e-${e.id ?? e.at}`, at: when(e.at), kind: "change", title: "Order cancelled", who: e.actor_name ?? e.actor });
    else if (e.action === "action:close") add({ key: `e-${e.id ?? e.at}`, at: when(e.at), kind: "change", title: "Order closed", who: e.actor_name ?? e.actor });
  }
  return out.sort((a, b) => b.at.localeCompare(a.at));
}

/** The newest `keep` entries, and how many older ones are folded away. */
export function fold(entries: Entry[], keep = 8): { shown: Entry[]; hidden: number } {
  return { shown: entries.slice(0, keep), hidden: Math.max(0, entries.length - keep) };
}
