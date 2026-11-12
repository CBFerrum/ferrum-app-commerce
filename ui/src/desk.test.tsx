// @vitest-environment jsdom
// The order desk's two dialogs driven in a page: changing an order (the difference before saving, what is
// sent, the server's refusal) and cancelling one (what goes back payment by payment). Requests and answers
// are those of the actions in src/edit.rs and src/refund.rs.

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act } from "react";
import { createRoot } from "react-dom/client";
import { MemoryRouter } from "react-router";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { CancelOrderDialog } from "./CancelOrder";
import { EditOrderDialog } from "./EditOrder";
import { ResendMailDialog } from "./ResendMail";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

const sent: { url: string; body: unknown }[] = [];
let refuse: string | null = null;
let refuseStatus = 422;
let refusePreview: { message: string; field?: string } | null = null;
/** What the preview route answers for a quantity of the Blue Mug taken from 2 to 1 on an order paid by card (src/edit.rs). */
const worked = (over: Record<string, unknown> = {}) => ({
  preview: true,
  total: "800",
  difference: "-250",
  owed: "0",
  payment_request: null,
  refund: null,
  settlement: { kind: "refund", amount: "250", gateway: "stripe", label: "Card", status: "Requested" },
  lines: [
    { line: 1, id: "a", item: "mug", name: "Blue Mug", qty: "1", rate: "250", amount: "250", delivery: false, delivered_qty: "0" },
    { line: 2, id: "b", item: "tee", name: "Logo Tee", qty: "1", rate: "400", amount: "400", delivery: false, delivered_qty: "1" },
    { line: 3, id: "c", item: "ship", name: "Shipping", qty: "1", rate: "150", amount: "150", delivery: true, delivered_qty: "0" },
  ],
  totals: { goods: "650", delivery: "150", taxes: "0", grand_total: "800", rounded_total: "800", was: "1050" },
  ...over,
});

beforeEach(() => {
  sent.length = 0;
  refuse = null;
  refuseStatus = 422;
  refusePreview = null;
  document.body.innerHTML = "";
  HTMLDialogElement.prototype.showModal ??= function (this: HTMLDialogElement) {
    this.setAttribute("open", "");
  };
  HTMLDialogElement.prototype.close ??= function (this: HTMLDialogElement) {
    this.removeAttribute("open");
  };
  vi.stubGlobal("fetch", async (url: string, init: { method?: string; body?: string } = {}) => {
    if (init.method === "POST" && String(url).endsWith("/desk/change-preview")) {
      sent.push({ url: String(url), body: init.body ? JSON.parse(init.body) : null });
      if (refusePreview) return new Response(JSON.stringify({ message: refusePreview.message, fields: refusePreview.field ? [{ field: refusePreview.field, message: refusePreview.message }] : [] }), { status: 422, headers: { "content-type": "application/json" } });
      return new Response(JSON.stringify(worked()), { status: 200, headers: { "content-type": "application/json" } });
    }
    if (init.method === "POST") {
      sent.push({ url: String(url), body: init.body ? JSON.parse(init.body) : null });
      if (refuse) return new Response(JSON.stringify({ message: refuse }), { status: refuseStatus, headers: { "content-type": "application/json" } });
      return new Response(JSON.stringify({ ...worked({ refund: "r1" }), cancelled: true, refunds: [], sent: true, state: "queued", mail: "Shipped", resent: "1", left_today: 1 }), { status: 200, headers: { "content-type": "application/json" } });
    }
    if (String(url).includes("/ShippingRule")) return new Response(JSON.stringify({ rows: [{ id: "r1", label: "Standard" }, { id: "r2", label: "Express" }], total: 2 }), { status: 200, headers: { "content-type": "application/json" } });
    return new Response(JSON.stringify({ rows: [{ id: "re1", reason: "Out of stock" }], total: 1 }), { status: 200, headers: { "content-type": "application/json" } });
  });
});
afterEach(() => vi.unstubAllGlobals());

async function mount(element: React.ReactElement) {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const host = document.createElement("div");
  document.body.append(host);
  await act(async () => {
    createRoot(host).render(
      <QueryClientProvider client={qc}>
        <MemoryRouter>{element}</MemoryRouter>
      </QueryClientProvider>,
    );
  });
  return host;
}
const text = () => (document.body.textContent ?? "").replace(/\s+/g, " ");
const press = async (el: Element | null) => void (await act(async () => (el as HTMLElement).click()));
const type = async (el: Element | null, value: string) =>
  void (await act(async () => {
    const input = el as HTMLInputElement;
    Object.getOwnPropertyDescriptor(Object.getPrototypeOf(input), "value")?.set?.call(input, value);
    input.dispatchEvent(new Event("input", { bubbles: true }));
  }));
/** The preview is asked for a moment after the last edit, and answered after that. */
const settle = async () => {
  await act(async () => void (await new Promise((r) => setTimeout(r, 400))));
  await act(async () => void (await new Promise((r) => setTimeout(r, 50))));
};
const button = (label: string) => [...document.querySelectorAll("button")].find((b) => b.textContent?.trim().startsWith(label)) ?? null;

const lines = [
  { id: "a", item: "mug", item_name: "Blue Mug", qty: "2", rate: "250", amount: "500", delivered_qty: "0" },
  { id: "b", item: "tee", item_name: "Logo Tee", qty: "1", rate: "400", amount: "400", delivered_qty: "1" },
  { id: "c", item: "ship", item_name: "Shipping", qty: "1", rate: "50", amount: "50", delivered_qty: "0" },
];
const stripe = { id: "p1", gateway: "stripe", amount: "1000", status: "Paid", currency_code: "INR", refunded_amount: "0" };

describe("changing an order", () => {
  const dialog = (over: Record<string, unknown> = {}) => (
    <EditOrderDialog order={{ id: "o1", number: "SO-1" }} lines={lines} shippingItem="ship" code="INR" onClose={() => {}} onDone={() => {}} {...over} />
  );

  it("asks the server what saving comes to, shows its lines, totals and settlement, then saves the same", async () => {
    const done = vi.fn();
    await mount(dialog({ onDone: done }));
    expect(text()).toContain("Change SO-1");
    expect(text()).toContain("Change a quantity, add an item or give a new address.");
    expect((button("Save the change") as HTMLButtonElement).disabled).toBe(true);
    expect(sent).toHaveLength(0);
    // The shipped line cannot be removed; the delivery charge is not edited.
    expect((document.querySelector('button[aria-label="Remove Logo Tee"]') as HTMLButtonElement).disabled).toBe(true);
    expect(text()).toContain("Priced again");
    await type(document.querySelector('input[aria-label="Quantity of Blue Mug"]'), "1");
    // The preview is asked for a moment after the last edit, as the save would be.
    await settle();
    const asked = sent.filter((x) => x.url.endsWith("/desk/change-preview"));
    expect(asked).toHaveLength(1);
    expect(asked[0].url).toBe("/api/app/commerce/desk/change-preview");
    expect(asked[0].body).toEqual({ sales_order: "o1", changes: '[{"line":1,"qty":"1"}]', notify: "No", approve: "No" });
    // The server's lines as priced, its totals, the difference and what happens to it.
    expect(text()).toContain("The order will cost less by");
    expect(text()).toContain("₹250.00");
    expect(text()).toContain("Before it was ₹1,050.00.");
    expect(text()).toContain("Shipping · Delivery");
    expect(text()).toContain("₹800.00");
    expect(text()).toContain("₹250.00 is sent back through Card (Stripe) as an adjustment refund.");
    await press(button("Save the change"));
    const saved = sent.find((x) => x.url.includes("/actions/web_update_items"));
    expect(saved?.body).toEqual({ changes: '[{"line":1,"qty":"1"}]', notify: "Yes", approve: "Yes" });
    expect(done).toHaveBeenCalled();
  });

  it("offers the delivery methods when the server says more than one applies, and asks again with the choice", async () => {
    refusePreview = { message: "more than one delivery method applies now: choose one", field: "shipping_rule" };
    await mount(dialog());
    await type(document.querySelector('input[aria-label="Quantity of Blue Mug"]'), "3");
    await settle();
    expect(text()).toContain("more than one delivery method applies now: choose one");
    expect((button("Save the change") as HTMLButtonElement).disabled).toBe(true);
    const select = document.querySelector("#edit-rule") as HTMLSelectElement;
    expect([...select.options].map((o) => o.textContent)).toEqual(["As the order has it", "Standard", "Express"]);
    refusePreview = null;
    await act(async () => {
      select.value = "r2";
      select.dispatchEvent(new Event("change", { bubbles: true }));
    });
    await settle();
    const last = sent.filter((x) => x.url.endsWith("/desk/change-preview")).at(-1);
    expect(last?.body).toMatchObject({ shipping_rule: "r2" });
    expect((button("Save the change") as HTMLButtonElement).disabled).toBe(false);
    await press(button("Save the change"));
    expect(sent.find((x) => x.url.includes("/actions/web_update_items"))?.body).toMatchObject({ shipping_rule: "r2" });
  });

  it("says a quantity below what shipped cannot be saved, and shows the server's refusal as it is", async () => {
    await mount(dialog());
    await type(document.querySelector('input[aria-label="Quantity of Logo Tee"]'), "0");
    expect(text()).toContain("1 of this line have shipped: it cannot go below that.");
    expect((button("Save the change") as HTMLButtonElement).disabled).toBe(true);
    await type(document.querySelector('input[aria-label="Quantity of Logo Tee"]'), "1");
    await type(document.querySelector('input[aria-label="Quantity of Blue Mug"]'), "3");
    await settle();
    refuse = "everything on this order has been delivered or it is closed";
    await press(button("Save the change"));
    expect(text()).toContain("everything on this order has been delivered or it is closed");
    expect((button("Save the change") as HTMLButtonElement).disabled).toBe(false);
  });

  it("does not offer a change to an order a gift card holds part of", async () => {
    await mount(dialog({ order: { id: "o1", number: "SO-1", commerce_card_held: "150" } }));
    expect(text()).toContain("A gift card holds part of this order until the rest is paid");
    await type(document.querySelector('input[aria-label="Quantity of Blue Mug"]'), "1");
    expect((button("Save the change") as HTMLButtonElement).disabled).toBe(true);
  });
});

describe("cancelling an order", () => {
  const rights = { ready: true, ships: true, asks: true, decides: true, takesPayment: true, settles: true, seesMoney: true, seesReturns: true, decidesReturns: true, editsCatalog: true };
  const cancel = (payments: unknown[], over: Record<string, unknown> = {}) => (
    <CancelOrderDialog order={{ id: "o1", number: "SO-1" }} payments={payments as never} rights={rights} code="INR" onClose={() => {}} onDone={() => {}} {...over} />
  );

  it("lists what goes back payment by payment, the card's part first, and cancels", async () => {
    const done = vi.fn();
    await mount(cancel([stripe, { id: "g1", gateway: "Gift Card", amount: "150", status: "Paid", currency_code: "INR" }, { id: "x", gateway: "stripe", amount: "5", status: "Expired" }], { onDone: done }));
    const rows = [...document.querySelectorAll("tbody tr")].map((r) => r.textContent ?? "");
    expect(rows).toHaveLength(2);
    expect(rows[0]).toContain("Gift card");
    expect(rows[0]).toContain("back onto the gift card now");
    expect(rows[1]).toContain("through Stripe");
    expect(text()).toContain("Cancel and send ₹1,150.00 back");
    await press(button("Cancel and send"));
    expect(sent[0].url).toContain("/actions/web_cancel");
    expect(sent[0].body).toEqual({ approve: "Yes" });
    expect(done).toHaveBeenCalled();
  });

  it("says an unpaid order is cancelled at once, and what an earlier change left owed", async () => {
    await mount(cancel([{ id: "i", gateway: "stripe", amount: "300", status: "Initiated" }], { owed: 100 }));
    expect(text()).toContain("Nothing has been paid on this order. It is cancelled at once");
    expect(text()).toContain("An earlier change left ₹100.00 owed by the shopper");
    expect(button("Cancel the order")).not.toBeNull();
  });

  it("leaves the decision to a manager for someone who may only ask, and shows the refusal", async () => {
    await mount(cancel([stripe], { rights: { ...rights, decides: false } }));
    expect(text()).toContain("You ask for these refunds and a manager approves them.");
    refuse = "part of this order has been delivered or invoiced";
    await press(button("Cancel and send"));
    expect(sent[0].body).toEqual({ approve: "No" });
    expect(text()).toContain("part of this order has been delivered or invoiced");
  });
});

describe("sending a mail again", () => {
  const all = { Shipped: true, "Payment Received": true, "Payment Failed": false, Refunded: false };
  const resend = (over: Record<string, unknown> = {}) => (
    <ResendMailDialog order={{ id: "o1", number: "SO-1", commerce_mails_resent: "2" }} available={all} email="asha@example.com" limit={3} onClose={() => {}} onDone={() => {}} {...over} />
  );

  it("offers the mails that may go again, the count and the daily limit, and sends the one chosen", async () => {
    const done = vi.fn();
    await mount(resend({ onDone: done }));
    expect(text()).toContain("at asha@example.com and nowhere else");
    expect(text()).toContain("Mails have been sent again 2 times on this order.");
    expect(text()).toContain("at most 3 times a day, the first send included");
    expect(text()).toContain("cannot be sent again: they hold a secret value");
    // Nothing failed and nothing was sent back, so those two cannot be chosen.
    expect((document.querySelector('input[value="Payment Failed"]') as HTMLInputElement).disabled).toBe(true);
    expect(text()).toContain("No payment on this order has failed.");
    await press(document.querySelector('input[value="Payment Received"]'));
    await press(button("Send it again"));
    expect(sent[0].url).toContain("/actions/web_resend_mail");
    expect(sent[0].body).toEqual({ mail: "Payment Received" });
    expect(done).toHaveBeenCalled();
  });

  it("shows the server's refusal as it is when the day's limit is reached", async () => {
    refuse = "this mail has gone to this order 3 times today: the limit is 3";
    refuseStatus = 409;
    const done = vi.fn();
    await mount(resend({ onDone: done }));
    await press(button("Send it again"));
    expect(text()).toContain("this mail has gone to this order 3 times today: the limit is 3");
    expect(done).not.toHaveBeenCalled();
    expect((button("Send it again") as HTMLButtonElement).disabled).toBe(false);
  });

  it("cannot be sent to an order with no address", async () => {
    await mount(resend({ email: null }));
    expect(text()).toContain("This order has no email address to write to.");
    expect((button("Send it again") as HTMLButtonElement).disabled).toBe(true);
  });
});
