// What the store's screens share: reading lists from the records API, who may do what, how many
// things wait for a decision, and the two things the queue and the order page both do to an order
// (mark its payment received, invoice it) with the words they ask for confirmation in.

import { useQuery } from "@tanstack/react-query";
import { ApiError, api, docPath, errorMessage, t } from "@ferrum/client";
import { confirmDialog, toast } from "@ferrum/kit";
import { can, useEntityMeta, useMe } from "@ferrum/records";
import { amountText, type Order, withMethods } from "./queues";

export const list = <T,>(entity: string, query: Record<string, string | number | boolean>, signal?: AbortSignal) => api.get<{ rows: T[]; total?: number }>(`/api/v1/${entity}`, query, signal);
export const f = (filters: unknown[]) => JSON.stringify(filters);

/** A list the person may not read is an empty list: a screen shows what it can. */
export const soft = <T,>(p: Promise<{ rows: T[] }>): Promise<T[]> =>
  p.then((r) => r.rows).catch((e) => {
    if (e instanceof ApiError && (e.status === 403 || e.status === 404)) return [];
    throw e;
  });

export const ORDER_FIELDS = "number,customer_name,posting_date,rounded_total,currency,status,docstatus,per_delivered,per_billed,commerce_payment_status,commerce_payment_method,commerce_contact_email";
export const WEB = ["commerce_web_order", "=", true];

/** The orders still being worked: open ones, and delivered ones whose money has not come in. */
export async function workingOrders(signal?: AbortSignal): Promise<Order[]> {
  const q = { fields: ORDER_FIELDS, titles: "true", sort: "posting_date asc", limit: 200 };
  const [open, unpaid] = await Promise.all([
    list<Order>("SalesOrder", { ...q, filters: f([WEB, ["docstatus", "=", 1], ["status", "in", ["To Deliver and Bill", "To Deliver", "To Bill"]]]) }, signal),
    list<Order>("SalesOrder", { ...q, limit: 100, filters: f([WEB, ["docstatus", "=", 1], ["status", "in", ["Completed", "Closed"]], ["commerce_payment_status", "=", "Pending"]]) }, signal),
  ]);
  const seen = new Set<string>();
  const orders = [...open.rows, ...unpaid.rows].filter((o) => !seen.has(o.id) && seen.add(o.id));
  // Orders from before the method was remembered on the order: ask their payment requests.
  const lacking = orders.filter((o) => !o.commerce_payment_method && o.commerce_payment_status !== "Not Required").map((o) => o.id);
  if (!lacking.length) return orders;
  const requests = await soft(list<{ sales_order?: string | null; gateway?: string | null }>("PaymentRequest", { filters: f([["sales_order", "in", lacking]]), fields: "sales_order,gateway", sort: "created_at desc", limit: 500 }, signal));
  return withMethods(orders, requests);
}

/** Roles that run the store's orders: the server holds the same list and refuses anyone else. */
const ORDER_ROLES = ["Website Manager", "Sales Manager", "Sales User"];

export interface Rights {
  /** The permissions have loaded: before that, nothing is known and nothing should be said. */
  ready: boolean;
  /** May ship and invoice web orders. */
  ships: boolean;
  /** May ask for a refund. */
  asks: boolean;
  /** May approve, decline and settle refunds. */
  decides: boolean;
  /** May record a payment received. */
  takesPayment: boolean;
  /** May decide on money a gateway could not book. */
  settles: boolean;
  /** Reads the refunds and the money a gateway could not book. */
  seesMoney: boolean;
  /** Reads return requests and the stock alerts. */
  seesReturns: boolean;
  /** Approves, declines and receives return requests. */
  decidesReturns: boolean;
  /** Changes the catalog: makes a product's variants. */
  editsCatalog: boolean;
}

export function useRights(): Rights {
  const me = useMe();
  const refundMeta = useEntityMeta("OrderRefund");
  const requestMeta = useEntityMeta("PaymentRequest");
  const exceptionMeta = useEntityMeta("PaymentException");
  const returnMeta = useEntityMeta("ReturnRequest");
  const returns = returnMeta.data?.permissions;
  const itemMeta = useEntityMeta("WebsiteItem");
  const refunds = refundMeta.data?.permissions;
  const requests = requestMeta.data?.permissions;
  const exceptions = exceptionMeta.data?.permissions;
  return {
    ready: !me.isPending && !refundMeta.isPending && !requestMeta.isPending && !exceptionMeta.isPending,
    ships: !!me.data && (me.data.user.is_admin || me.data.user.roles.some((r) => ORDER_ROLES.includes(r))),
    asks: can(refunds, "create"),
    decides: can(refunds, "write"),
    takesPayment: can(requests, "write"),
    settles: can(exceptions, "write"),
    seesMoney: can(refunds, "read") || can(exceptions, "read"),
    seesReturns: can(returns, "read"),
    decidesReturns: can(returns, "write"),
    editsCatalog: can(itemMeta.data?.permissions, "write"),
  };
}

/** How many refunds and payments wait for a person: the badge on the Money tab. */
export function useWaiting(): number | undefined {
  const rights = useRights();
  const q = useQuery({
    enabled: rights.seesMoney,
    queryKey: ["commerce", "waiting"],
    queryFn: async () => {
      const total = (entity: string, filters: unknown[]) => list(entity, { filters: f(filters), limit: 1, count: "true", fields: "id" }).then((r) => r.total ?? 0).catch(() => 0);
      const counts = await Promise.all([
        total("OrderRefund", [["status", "in", ["Requested", "Refund Failed"]]]),
        total("OrderRefund", [["status", "=", "Refunded"], ["booked", "=", false]]),
        total("PaymentException", [["status", "in", ["Open", "Refund Failed"]]]),
      ]);
      return counts.reduce((a, b) => a + b, 0);
    },
  });
  return q.data;
}

interface Request {
  id: string;
  amount?: string | null;
  currency_code?: string | null;
  gateway?: string | null;
}

/** Records that the payment of an order came in (cash on delivery, a bank transfer), after asking. */
export async function markPaid(order: { id: string; number?: string | null }): Promise<boolean> {
  try {
    const found = await list<Request>("PaymentRequest", { filters: f([["sales_order", "=", order.id], ["status", "in", ["Initiated", "Failed"]]]), fields: "amount,currency_code,gateway", sort: "created_at desc", limit: 1 });
    const request = found.rows[0];
    if (!request) {
      toast(t("There is no open payment request on this order."), "error");
      return false;
    }
    const ok = await confirmDialog({
      title: t("Mark {value} as paid?", { value: order.number ?? t("this order") }),
      message: (request.gateway ? t("{value} received by {gateway}. The payment goes into the books.", { value: amountText(request.amount, request.currency_code), gateway: request.gateway }) : t("{value} received. The payment goes into the books.", { value: amountText(request.amount, request.currency_code) })),
      confirmLabel: t("Mark paid"),
    });
    if (!ok) return false;
    await api.post(docPath("PaymentRequest", request.id, "actions", "mark_paid"), { amount: request.amount, currency: request.currency_code });
    toast(t("Payment recorded"));
    return true;
  } catch (e) {
    toast(errorMessage(e), "error");
    return false;
  }
}

/** Invoices a delivered order, after asking. */
export async function invoiceOrder(order: { id: string; number?: string | null }): Promise<boolean> {
  const ok = await confirmDialog({ title: t("Invoice {value}?", { value: order.number ?? t("this order") }), message: t("The invoice is made and posted. A payment already received is applied to it."), confirmLabel: t("Create invoice") });
  if (!ok) return false;
  try {
    await api.post(docPath("SalesOrder", order.id, "actions", "web_invoice"), {});
    toast(t("Invoice created"));
    return true;
  } catch (e) {
    toast(errorMessage(e), "error");
    return false;
  }
}

/** Puts text on the clipboard and says so; where the browser refuses, says that. */
export async function copy(text: string, what: string): Promise<void> {
  try {
    await navigator.clipboard.writeText(text);
    toast(t("{what} copied", { what }));
  } catch {
    toast(t("Your browser would not copy {what}. Select it and copy it by hand.", { what: what.toLowerCase() }), "error");
  }
}

/** The link a shopper follows an order by, made by the server. */
export async function trackingLink(order: string): Promise<string | null> {
  const r = await api.post<{ link: string | null }>(docPath("SalesOrder", order, "actions", "web_tracking_link"), {});
  return r.link;
}

/** How many return requests wait for the desk to decide: the badge on the Returns tab. */
export function useReturnsWaiting(): number | undefined {
  const rights = useRights();
  const q = useQuery({
    enabled: rights.seesReturns,
    queryKey: ["commerce", "returns-waiting"],
    queryFn: () => list("ReturnRequest", { filters: f([["status", "=", "Requested"]]), limit: 1, count: "true", fields: "id" }).then((r) => r.total ?? 0).catch(() => 0),
  });
  return q.data;
}
