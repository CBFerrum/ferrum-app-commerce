// The return-and-refund dialog's choices, kept apart so they are tested without a browser: what is
// asked of the server, how a quantity is kept within what may come back, and when the person may go
// ahead. The server decides what may come back and what it comes to (the plan); this only carries
// the person's choices to it and back.

export interface PlanLine {
  id: string;
  item?: string | null;
  name?: string | null;
  qty: string;
  delivered: string;
  invoiced: boolean;
  returnable: string;
  rate: string;
}

export interface Plan {
  payable: boolean;
  why?: string;
  currency?: string;
  payment?: { gateway?: string | null; label?: string | null; reference?: string | null; paid: string; held: string; refundable: string; offline: boolean };
  cancel?: { allowed: boolean };
  lines?: PlanLine[];
  shipping?: { available: boolean };
  estimate?: { amount: string; errors: string[] } | null;
}

export type Kind = "Return" | "Cancellation";

export interface Choice {
  /** Units chosen per order line. */
  qty: Record<string, number>;
  /** Lines whose "back on the shelf" the person set themselves; the rest follow the reason. */
  restock: Record<string, boolean>;
  shipping: boolean;
}

export const emptyChoice = (): Choice => ({ qty: {}, restock: {}, shipping: false });

/** A quantity typed or stepped, kept between none and what may come back (three decimals at most,
 *  for units that are not whole). */
export function clampQty(raw: unknown, max: number): number {
  const n = Number(raw);
  if (!Number.isFinite(n) || n <= 0) return 0;
  return Math.min(Math.round(n * 1000) / 1000, Math.max(0, max));
}

/** Whether a line goes back on the shelf: what the person set, else what the reason usually means. */
export const restocks = (c: Choice, line: string, reasonRestocks: boolean) => c.restock[line] ?? reasonRestocks;

/** The lines with units chosen. */
export const chosen = (c: Choice) => Object.entries(c.qty).filter(([, q]) => q > 0);

/** What the server is asked: for the plan (an estimate) and for the refund itself. */
export function request(order: string, kind: Kind, c: Choice, reasonRestocks: boolean) {
  if (kind === "Cancellation") return { sales_order: order, kind };
  return {
    sales_order: order,
    kind,
    include_shipping: c.shipping,
    lines: chosen(c).map(([line, qty]) => ({ sales_order_line: line, qty: String(qty), restock: restocks(c, line, reasonRestocks) })),
  };
}

/** Whether the person may go ahead, and if not, what is missing. */
export function readiness(o: { plan: Plan | undefined; kind: Kind; choice: Choice; reason: string | null; estimating: boolean }): { ok: boolean; why: string } {
  const { plan, kind, choice, reason, estimating } = o;
  if (!plan?.payable) return { ok: false, why: plan?.why ?? "" };
  if (kind === "Cancellation") {
    if (!plan.cancel?.allowed) return { ok: false, why: "This order has shipped or is already being refunded: send the goods back instead." };
  } else if (!chosen(choice).length) {
    return { ok: false, why: "Choose what comes back." };
  }
  if (!reason) return { ok: false, why: "Say why." };
  if (estimating) return { ok: false, why: "Working out the amount…" };
  const e = plan.estimate;
  if (!e) return { ok: false, why: "Working out the amount…" };
  if (e.errors.length) return { ok: false, why: e.errors[0] };
  if (!(Number(e.amount) > 0)) return { ok: false, why: "There is nothing to send back." };
  return { ok: true, why: "" };
}
