// Each screen drawn once with the data it would have, the way a server would draw it (no browser
// here): what a person would read in every state it has. It catches a screen that cannot draw with
// real-shaped data, which the types alone do not.

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { renderToStaticMarkup } from "react-dom/server";
import { MemoryRouter, Route, Routes } from "react-router";
import { describe, expect, it } from "vitest";
import { todayIso } from "@ferrum/client";
import { RefundRow } from "./Decisions";
import { Desk } from "./Desk";
import { CommerceHome } from "./Home";
import { Money } from "./Money";
import { OrderRoute } from "./Order";
import { RefundDialog } from "./Refund";
import { ShipDialog } from "./Ship";
import type { Plan } from "./choices";

const perms = (write: "all" | "none") => ({ read: "all", write, create: write, delete: "none", submit: "none", cancel: "none", export: "none", import: "none" });
const me = { tenant: "t", user: { id: "u", email: "m@shop.test", full_name: "M", is_admin: false, roles: ["Website Manager"] }, default_company: null, scopes: {}, fiscal_year_start_month: 4, packages: ["commerce"] };

function client(seed: [unknown[], unknown][]) {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: Infinity, gcTime: Infinity } } });
  qc.setQueryData(["me"], me);
  for (const name of ["OrderRefund", "PaymentRequest", "PaymentException"]) qc.setQueryData(["meta", name], { name, permissions: perms("all") });
  for (const [key, data] of seed) qc.setQueryData(key, data);
  return qc;
}

function draw(qc: QueryClient, at: string, path: string, element: React.ReactElement) {
  return renderToStaticMarkup(
    <QueryClientProvider client={qc}>
      <MemoryRouter initialEntries={[at]}>
        <Routes>
          <Route path={path} element={element} />
        </Routes>
      </MemoryRouter>
    </QueryClientProvider>,
  );
}

const plain = (html: string) => html.replace(/<[^>]*>/g, " ").replace(/\s+/g, " ").trim();

const order = (o: Record<string, unknown>): Record<string, unknown> => ({ id: "o1", number: "SO-1", customer_name: "Asha Rao", posting_date: "2026-10-30", rounded_total: "1121", status: "To Deliver and Bill", docstatus: 1, per_delivered: "0", per_billed: "0", commerce_payment_status: "Paid", commerce_payment_method: "stripe", commerce_web_order: true, _titles: { currency: "INR" }, ...o });

describe("the desk", () => {
  it("says what waits in each queue, with the one thing to do", () => {
    const orders = [order({}), order({ id: "o2", number: "SO-2", commerce_payment_status: "Pending", customer_name: "Ravi" })];
    const qc = client([[["commerce", "desk"], orders]]);
    const ship = plain(draw(qc, "/orders", "/orders", <Desk />));
    expect(ship).toContain("SO-1");
    expect(ship).toContain("Asha Rao");
    expect(ship).toContain("Ship");
    expect(ship).toContain("Paid. Pack it and hand it to the carrier.");
    expect(ship).not.toContain("SO-2");
    expect(ship).toMatch(/To pay\s*1/);
    expect(plain(draw(qc, "/orders?stage=pay", "/orders", <Desk />))).toContain("Waiting for stripe to confirm");
    expect(plain(draw(qc, "/orders?stage=invoice", "/orders", <Desk />))).toContain("Nothing to invoice");
    expect(plain(draw(qc, "/orders?q=zzz", "/orders", <Desk />))).toContain("No order matches");
  });

  it("shows the loading state before the orders are there", () => {
    const qc = client([]);
    expect(draw(qc, "/orders", "/orders", <Desk />)).toContain('role="status" aria-label="Loading…"');
  });
});

describe("money that needs a person", () => {
  it("shows what waits, and what is done", () => {
    const refunds = [
      { id: "r1", number: "REFUND-1", order_number: "SO-1", sales_order: "o1", kind: "Return", amount: "354", currency_code: "INR", gateway: "stripe", status: "Requested", requested_by: "clerk@shop.test", created_at: "2026-11-01T09:00:00Z" },
      { id: "r2", number: "REFUND-2", order_number: "SO-3", sales_order: "o3", kind: "Cancellation", amount: "300", currency_code: "INR", gateway: "stripe", status: "Refunded", booked: false, booking_error: "The period is closed.", created_at: "2026-11-01T09:00:00Z" },
      { id: "r3", number: "REFUND-3", order_number: "SO-4", amount: "10", gateway: "stripe", status: "Refunded", booked: true, created_at: "2026-11-01T09:00:00Z" },
    ];
    const exceptions = [{ id: "x1", gateway: "paypal", gateway_reference: "PAY-1", payment_request: "p1", amount: "1149.88", currency_code: "INR", reason: "It was paid after the request closed.", status: "Open", created_at: "2026-11-01T09:00:00Z", _titles: { payment_request: "SO-9" } }];
    const qc = client([[["commerce", "money"], { refunds, exceptions, orders: new Map([["p1", { id: "p1", sales_order: "o9", order_number: "SO-9" }]]) }]]);
    const decide = plain(draw(qc, "/money", "/money", <Money />));
    expect(decide).toContain("3 waiting for a decision");
    expect(decide).toContain("Waiting for a decision");
    expect(decide).toContain("clerk@shop.test asked to send");
    expect(decide).toContain("Sent back, but not in the books yet");
    expect(decide).toContain("The period is closed.");
    expect(decide).toContain("Taken, but could not be booked");
    expect(decide).toContain("Approve refund");
    expect(decide).toContain("Try posting again");
    expect(decide).toContain("Payments the gateway took and could not book");
    const done = plain(draw(qc, "/money?tab=done", "/money", <Money />));
    expect(done).toContain("Sent back");
    expect(done).not.toContain("clerk@shop.test");
    expect(plain(draw(qc, "/money?tab=waiting", "/money", <Money />))).toContain("Nothing is with a gateway");
  });
});

describe("an order", () => {
  const related = {
    payments: [{ id: "p1", gateway: "stripe", amount: "1121", currency_code: "INR", status: "Paid", paid_at: "2026-11-01T09:02:00Z", gateway_reference: "pi_1", created_at: "2026-11-01T09:00:05Z", refunded_amount: "354" }],
    deliveries: [{ id: "n1", number: "DN-1", posting_date: "2026-11-01", created_at: "2026-11-01T10:00:00Z", transporter: "BlueDart", commerce_tracking_number: "BD123", commerce_tracking_url: "https://t.example/BD123" }],
    invoices: [{ id: "i1", number: "SINV-1", posting_date: "2026-11-01", created_at: "2026-11-01T10:00:02Z", rounded_total: "1121", status: "Paid" }],
    refunds: [{ id: "r1", number: "REFUND-1", kind: "Return", amount: "354", currency_code: "INR", gateway: "stripe", status: "Approved", requested_by: "m@shop.test", approved_by: "m@shop.test", approved_at: new Date().toISOString(), created_at: "2026-11-02T09:00:00Z" }],
    exceptions: [],
    audit: [],
  };
  const full = order({
    created_at: "2026-11-01T09:00:00Z",
    items: [{ id: "l1", item: "mug", item_name: "Blue Mug", qty: "2", rate: "250", amount: "500", delivered_qty: "2" }, { id: "l2", item: "ship", item_name: "Shipping", qty: "1", rate: "50", amount: "50", delivered_qty: "1" }],
    taxes: [{ description: "VAT", tax_amount: "99" }],
    total: "550",
    per_delivered: "100",
    per_billed: "100",
    status: "Completed",
    commerce_contact_email: "asha@example.com",
    commerce_shipping_address: "Asha Rao\n12 MG Road\nBengaluru, Karnataka 560001\nIN",
    commerce_notes: "Leave it with the guard",
    customer: "c1",
  });

  it("tells the order, its payment, its shipment, its refund and its story", () => {
    const qc = client([[["commerce", "order", "o1"], full], [["commerce", "order", "o1", "related"], related]]);
    const html = plain(draw(qc, "/o/o1", "/o/:id", <OrderRoute />));
    for (const expected of ["SO-1", "Blue Mug", "Shipping", "VAT", "via", "pi_1", "BlueDart · BD123", "Track it", "SINV-1", "Approved: Stripe is sending it", "Leave it with the guard", "12 MG Road", "asha@example.com", "What happened", "Order placed", "Send money back", "Nothing left to do."]) {
      expect(html, expected).toContain(expected);
    }
  });

  it("is a cancelled order when it is", () => {
    const qc = client([[["commerce", "order", "o1"], { ...full, docstatus: 2, status: "Cancelled" }], [["commerce", "order", "o1", "related"], { ...related, deliveries: [], invoices: [], refunds: [] }]]);
    const html = plain(draw(qc, "/o/o1", "/o/:id", <OrderRoute />));
    expect(html).toContain("This order was cancelled.");
    expect(html).toContain("Nothing shipped.");
  });

  it("offers to ship an order that is paid, and says who ships when you cannot", () => {
    const todo = { ...full, per_delivered: "0", per_billed: "0", status: "To Deliver and Bill", items: (full.items as { delivered_qty: string }[]).map((l) => ({ ...l, delivered_qty: "0" })) };
    const qc = client([[["commerce", "order", "o1"], todo], [["commerce", "order", "o1", "related"], { ...related, deliveries: [], invoices: [], refunds: [] }]]);
    const html = plain(draw(qc, "/o/o1", "/o/:id", <OrderRoute />));
    expect(html).toContain("Next:");
    expect(html).toContain("Paid. Pack it and hand it to the carrier.");
    expect(html).toContain("In stock for this order");
    expect(html).toMatch(/Ship/);
  });

  it("knows an older cash-on-delivery order, placed before orders remembered how they are paid, is ready to ship", () => {
    const old = { ...full, per_delivered: "0", per_billed: "0", status: "To Deliver and Bill", commerce_payment_status: "Pending", commerce_payment_method: null, items: (full.items as { delivered_qty: string }[]).map((l) => ({ ...l, delivered_qty: "0" })) };
    const asked = [{ id: "p1", gateway: "Cash on Delivery", amount: "1121", currency_code: "INR", status: "Initiated", created_at: "2026-11-01T09:00:05Z" }];
    const qc = client([[["commerce", "order", "o1"], old], [["commerce", "order", "o1", "related"], { ...related, payments: asked, deliveries: [], invoices: [], refunds: [] }]]);
    const html = plain(draw(qc, "/o/o1", "/o/:id", <OrderRoute />));
    expect(html).toContain("Cash on delivery: the courier collects the money.");
    expect(html).not.toContain("Waiting for");
  });

  it("offers to change and cancel an order that has not shipped, and says what it owes and what a card holds", () => {
    const todo = { ...full, per_delivered: "0", per_billed: "0", status: "To Deliver and Bill", commerce_amount_owed: "120", commerce_card_held: "150", items: (full.items as { delivered_qty: string }[]).map((l) => ({ ...l, delivered_qty: "0" })) };
    const qc = client([[["commerce", "order", "o1"], todo], [["commerce", "order", "o1", "related"], { ...related, deliveries: [], invoices: [], refunds: [] }]]);
    const html = plain(draw(qc, "/o/o1", "/o/:id", <OrderRoute />));
    expect(html).toContain("Change…");
    expect(html).toContain("Cancel order…");
    expect(html).toContain("the shopper still owes");
    expect(html).toContain("A gift card holds");
    // Shipped in full: nothing to cancel, nothing to change.
    const sent = plain(draw(client([[["commerce", "order", "o1"], full], [["commerce", "order", "o1", "related"], related]]), "/o/o1", "/o/:id", <OrderRoute />));
    expect(sent).not.toContain("Cancel order…");
    expect(sent).not.toContain("Change…");
  });

  it("is not for an order that did not come from the web", () => {
    const qc = client([[["commerce", "order", "o1"], order({ commerce_web_order: false })]]);
    expect(plain(draw(qc, "/o/o1", "/o/:id", <OrderRoute />))).toContain("This is not a web order");
  });
});

describe("the return and refund dialog", () => {
  const plan: Plan = {
    payable: true,
    currency: "INR",
    payment: { gateway: "stripe", label: "Card", reference: "pi_1", paid: "1121", held: "0", refundable: "1121", offline: false },
    cancel: { allowed: false },
    lines: [
      { id: "l1", item: "mug", name: "Blue Mug", qty: "2", delivered: "2", invoiced: true, returnable: "2", rate: "250" },
      { id: "l2", item: "tee", name: "Logo Tee", qty: "1", delivered: "1", invoiced: true, returnable: "0", rate: "400" },
    ],
    shipping: { available: true },
    estimate: null,
  };
  const asked = JSON.stringify({ sales_order: "o1", kind: "Return", include_shipping: false, lines: [] });
  const rights = { ready: true, ships: true, asks: true, decides: true, takesPayment: true, settles: true, seesMoney: true, seesReturns: true, decidesReturns: true, editsCatalog: true };

  it("offers the lines that can come back, with what is left of each, and what it comes to", () => {
    const qc = client([[["commerce", "reasons"], [{ id: "re1", reason: "Changed their mind", restock: true }]], [["commerce", "refund-plan", asked], plan]]);
    const html = renderToStaticMarkup(
      <QueryClientProvider client={qc}>
        <RefundDialog order={{ id: "o1", number: "SO-1" }} rights={rights} onClose={() => {}} onDone={() => {}} />
      </QueryClientProvider>,
    );
    const text = plain(html);
    for (const expected of ["Send money back", "Blue Mug", "2 delivered", "2 can come back", "Logo Tee", "all back already", "Refund the delivery charge too", "Changed their mind", "Card (Stripe)", "1,121.00", "Choose what comes back."]) {
      expect(text, expected).toContain(expected);
    }
    expect(html).toContain("Number of Blue Mug to send back");
  });

  it("says why an order has nothing to send back", () => {
    const qc = client([[["commerce", "reasons"], []], [["commerce", "refund-plan", asked], { payable: false, why: "Nothing has been paid on this order, so there is no money to send back." } satisfies Plan]]);
    const html = renderToStaticMarkup(
      <QueryClientProvider client={qc}>
        <RefundDialog order={{ id: "o1", number: "SO-1" }} rights={rights} onClose={() => {}} onDone={() => {}} />
      </QueryClientProvider>,
    );
    expect(plain(html)).toContain("Nothing has been paid on this order");
  });
});

describe("the home", () => {
  it("puts what needs a decision first, then the day", () => {
    // The page counts today on the workspace's day, so the test does too.
    const day = todayIso();
    const entity = (name: string, read = "all") => ({ name, label: name, label_plural: name, permissions: perms(read === "all" ? "all" : "none") });
    const qc = client([
      [["meta"], { version: 1, packages: [], modules: [], entities: ["SalesOrder", "WebsiteItem", "PaymentRequest", "ItemReview"].map((n) => entity(n)), warnings: [] }],
      [["commerce", "waiting"], 2],
      [["commerce", "sales", day], [{ day, orders: 3, revenue: "1500" }]],
      [["commerce", "desk"], [order({}), order({ id: "o2", number: "SO-2", commerce_payment_status: "Pending", customer_name: "Ravi" })]],
      [["commerce", "payments"], [{ id: "p1", sales_order: "o2", order_number: "SO-2", payer_name: "Ravi", gateway: "stripe", amount: "100", currency_code: "INR", status: "Expired" }]],
      [["commerce", "reviews"], { rows: [], total: 0 }],
      [["commerce", "catalog"], [4, 1, 3]],
    ]);
    const html = plain(draw(qc, "/", "/", <CommerceHome />));
    for (const expected of ["2 refunds and payments need a decision", "Decide", "Today", "3 orders", "Orders at the desk", "To ship", "Waiting longest to be sent", "SO-1", "Payments that did not go through", "SO-2", "Expired", "No reviews waiting.", "4 published", "1 not published", "3 variants", "on its form, say which item it is a variant of"]) {
      expect(html, expected).toContain(expected);
    }
  });
});

describe("shipping an order", () => {
  it("asks for the carrier and the link, and says what shipping does", () => {
    const qc = client([]);
    const html = renderToStaticMarkup(
      <QueryClientProvider client={qc}>
        <ShipDialog order={{ id: "o1", number: "SO-1", customer_name: "Asha", commerce_payment_method: "Cash on Delivery", commerce_payment_status: "Pending" }} onClose={() => {}} onDone={() => {}} />
      </QueryClientProvider>,
    );
    const text = plain(html);
    for (const expected of ["Ship SO-1", "Everything still to deliver on this order leaves the warehouse for Asha.", "Cash on delivery: the courier collects the money", "Carrier", "Tracking number", "Tracking link", "Tell the shopper it has shipped", "Create the invoice too"]) {
      expect(text, expected).toContain(expected);
    }
  });
});

describe("a refund card", () => {
  const rights = { ready: true, ships: true, asks: true, decides: true, takesPayment: true, settles: true, seesMoney: true, seesReturns: true, decidesReturns: true, editsCatalog: true };
  const card = (r: Record<string, unknown>, who = rights) =>
    plain(
      renderToStaticMarkup(
        <QueryClientProvider client={client([])}>
          <MemoryRouter>
            <RefundRow r={{ id: "r1", number: "REFUND-1", order_number: "SO-1", sales_order: "o1", amount: "354", currency_code: "INR", gateway: "stripe", ...r }} rights={who} onChange={() => {}} />
          </MemoryRouter>
        </QueryClientProvider>,
      ),
    );

  it("offers a manager the next step of each state", () => {
    expect(card({ status: "Requested" })).toContain("Approve");
    expect(card({ status: "Refund Failed", refund_error: "card closed" })).toContain("Approve again");
    expect(card({ status: "Refund Failed", refund_error: "card closed" })).toContain("card closed");
    expect(card({ status: "Approved", gateway: "Cash on Delivery" })).toContain("I refunded it");
    expect(card({ status: "Requested", gateway: "Manual" })).toContain("I refunded it");
    expect(card({ status: "Refunded", booked: false, booking_error: "closed" })).toContain("Try posting again");
    expect(card({ status: "Refunded", booked: true })).not.toContain("Try posting again");
    expect(card({ status: "Declined", decision_note: "Not eligible" })).toContain("Not eligible");
  });

  it("tells someone who cannot decide that a manager does", () => {
    const text = card({ status: "Requested" }, { ...rights, decides: false });
    expect(text).toContain("A manager decides this.");
    expect(text).not.toContain("Approve");
  });
});
