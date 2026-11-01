import { describe, expect, it } from "vitest";
import { ago, amountText, counts, nextStep, type Order, pickList, queue, stage, statusWords, units, waitedDays, withMethods } from "./queues";

const order = (o: Partial<Order>): Order => ({ id: "1", docstatus: 1, status: "To Deliver and Bill", per_delivered: 0, per_billed: 0, commerce_payment_status: "Pending", commerce_payment_method: "stripe", ...o });

describe("the web-order desk", () => {
  it("sorts an order into the queue where it waits", () => {
    expect(stage(order({}))).toBe("pay");
    expect(stage(order({ commerce_payment_status: "Paid" }))).toBe("ship");
    expect(stage(order({ commerce_payment_status: "Not Required", commerce_payment_method: "Manual" }))).toBe("ship");
    expect(stage(order({ commerce_payment_method: "Cash on Delivery" }))).toBe("ship");
    expect(stage(order({ commerce_payment_method: "Manual" }))).toBe("pay");
    expect(stage(order({ commerce_payment_status: "Paid", per_delivered: 100, status: "To Bill" }))).toBe("invoice");
    expect(stage(order({ commerce_payment_status: "Paid", per_delivered: "100", per_billed: "100", status: "Completed" }))).toBe("done");
    expect(stage(order({ status: "Cancelled", docstatus: 2 }))).toBe("done");
  });

  it("puts a delivered cash order back in the queue for money until the cash is recorded", () => {
    const sent = { per_delivered: 100, per_billed: 100, status: "Completed", commerce_payment_method: "Cash on Delivery" };
    expect(stage(order({ ...sent, status: "To Bill" }))).toBe("pay");
    expect(stage(order({ ...sent, status: "To Bill", commerce_payment_status: "Paid" }))).toBe("done");
  });

  it("names one next step, and says why when there is none", () => {
    expect(nextStep(order({ commerce_payment_status: "Paid" }))).toMatchObject({ step: "ship", label: "Ship" });
    expect(nextStep(order({ commerce_payment_method: "Cash on Delivery" })).why).toMatch(/courier collects/);
    expect(nextStep(order({ commerce_payment_status: "Paid", per_delivered: 100 }))).toMatchObject({ step: "invoice", label: "Create invoice" });
    expect(nextStep(order({ commerce_payment_method: "Manual" }))).toMatchObject({ step: "mark_paid", label: "Mark paid" });
    const delivered = nextStep(order({ commerce_payment_method: "Cash on Delivery", per_delivered: 100, per_billed: 100 }));
    expect(delivered).toMatchObject({ step: "mark_paid" });
    expect(nextStep(order({})).step).toBe("wait");
    expect(nextStep(order({})).why).toMatch(/stripe/);
    expect(nextStep(order({ commerce_payment_status: "Expired" })).why).toMatch(/ran out of time/);
    expect(nextStep(order({ commerce_payment_status: "Failed" })).why).toMatch(/failed/);
    expect(nextStep(order({ status: "Cancelled", docstatus: 2 })).why).toBe("Cancelled.");
    expect(nextStep(order({ status: "Completed", commerce_payment_status: "Paid", per_delivered: 100, per_billed: 100 })).why).toBe("Nothing left to do.");
  });

  it("counts the queues and lists the longest waiting first", () => {
    const orders = [
      order({ id: "a", number: "SO-2", posting_date: "2026-10-31", commerce_payment_status: "Paid" }),
      order({ id: "b", number: "SO-1", posting_date: "2026-10-29", commerce_payment_status: "Paid" }),
      order({ id: "c", number: "SO-3", posting_date: "2026-10-30", customer_name: "Asha Rao" }),
      order({ id: "d", status: "Completed", per_delivered: 100, per_billed: 100, commerce_payment_status: "Paid" }),
    ];
    expect(counts(orders)).toEqual({ pay: 1, ship: 2, invoice: 0, done: 1 });
    expect(queue(orders, "ship").map((o) => o.id)).toEqual(["b", "a"]);
    expect(queue(orders, "all").length).toBe(4);
    expect(queue(orders, "all", " asha ").map((o) => o.id)).toEqual(["c"]);
    expect(queue(orders, "ship", "so-2").map((o) => o.id)).toEqual(["a"]);
  });

  it("says how long ago in the units a person uses", () => {
    const now = new Date(2026, 10, 1, 12, 0, 0);
    expect(ago(new Date(2026, 10, 1, 11, 59, 40).toISOString(), now)).toBe("just now");
    expect(ago(new Date(2026, 10, 1, 11, 15, 0).toISOString(), now)).toBe("45 min");
    expect(ago(new Date(2026, 10, 1, 9, 0, 0).toISOString(), now)).toBe("3 h");
    expect(ago("2026-10-31", now)).toBe("1 day");
    expect(ago("2026-10-27", now)).toBe("5 days");
    expect(ago(null, now)).toBe("");
    expect(ago("not a date", now)).toBe("");
    expect(waitedDays(order({ posting_date: "2026-10-29" }), now)).toBe(3);
    expect(waitedDays(order({ posting_date: "2026-11-05" }), now)).toBe(0);
  });

  it("says the order's state in words", () => {
    expect(statusWords(order({}))).toBe("To deliver");
    expect(statusWords(order({ status: "To Bill" }))).toBe("To invoice");
    expect(statusWords(order({ status: "Completed" }))).toBe("Completed");
    expect(statusWords(order({ docstatus: 2, status: "Cancelled" }))).toBe("Cancelled");
  });

  it("makes a pick list: each item once per shelf, what is still to send, and which orders want it", () => {
    const orders = [
      { id: "1", number: "SO-1", items: [{ item: "mug", item_name: "Blue Mug", qty: 2, delivered_qty: 0, warehouse: "w1", _titles: { warehouse: "Stores" } }, { item: "tee", item_name: "Logo Tee", qty: "1", delivered_qty: "1", warehouse: "w1", _titles: { warehouse: "Stores" } }] },
      { id: "2", number: "SO-2", items: [{ item: "mug", item_name: "Blue Mug", qty: 1, warehouse: "w1", _titles: { warehouse: "Stores" } }, { item: "ship", item_name: "Shipping", qty: 1, warehouse: "w1", _titles: { warehouse: "Stores" } }] },
    ];
    const list = pickList(orders, (l) => l.item !== "ship");
    expect(list).toEqual([{ key: "mug|w1", item: "Blue Mug", warehouse: "Stores", qty: 3, orders: ["SO-1", "SO-2"] }]);
    expect(pickList([])).toEqual([]);
  });

  it("writes amounts and units plainly", () => {
    expect(amountText(1149.88, "INR", "en-IN")).toMatch(/1,149\.88/);
    expect(amountText("12", "ZZ", "en-US")).toBe("ZZ 12.00");
    expect(amountText("x", "INR")).toBe("");
    expect(units(2)).toBe("2");
    expect(units(0.5)).toBe("0.5");
    expect(units(2.0004)).toBe("2");
  });

  it("gives an older order the method of its latest payment request", () => {
    const old = order({ id: "a", commerce_payment_method: null, commerce_payment_status: "Pending" });
    const known = order({ id: "b", commerce_payment_method: "stripe" });
    const filled = withMethods([old, known, order({ id: "c", commerce_payment_method: null })], [
      { sales_order: "a", gateway: "Cash on Delivery" },
      { sales_order: "a", gateway: "stripe" },
      { sales_order: "b", gateway: "paypal" },
    ]);
    expect(filled.map((o) => o.commerce_payment_method)).toEqual(["Cash on Delivery", "stripe", null]);
    expect(stage(filled[0])).toBe("ship");
  });
});
