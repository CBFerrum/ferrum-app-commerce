import { describe, expect, it } from "vitest";
import { type Choice, chosen, clampQty, emptyChoice, type Plan, readiness, request, restocks } from "./choices";

const plan = (o: Partial<Plan> = {}): Plan => ({ payable: true, currency: "INR", cancel: { allowed: false }, lines: [], estimate: { amount: "354", errors: [] }, ...o });
const picked = (qty: Record<string, number>, rest: Partial<Choice> = {}): Choice => ({ ...emptyChoice(), qty, ...rest });

describe("the return and refund dialog", () => {
  it("keeps a quantity between none and what may come back", () => {
    expect(clampQty(3, 2)).toBe(2);
    expect(clampQty("1", 2)).toBe(1);
    expect(clampQty(-1, 2)).toBe(0);
    expect(clampQty("abc", 2)).toBe(0);
    expect(clampQty(0.12345, 2)).toBe(0.123);
    expect(clampQty(1, 0)).toBe(0);
  });

  it("asks the server for what the person chose, and only that", () => {
    const c = picked({ a: 2, b: 0, c: 1 }, { shipping: true, restock: { c: false } });
    expect(chosen(c)).toEqual([["a", 2], ["c", 1]]);
    expect(request("o1", "Return", c, true)).toEqual({
      sales_order: "o1",
      kind: "Return",
      include_shipping: true,
      lines: [
        { sales_order_line: "a", qty: "2", restock: true },
        { sales_order_line: "c", qty: "1", restock: false },
      ],
    });
    expect(request("o1", "Cancellation", c, true)).toEqual({ sales_order: "o1", kind: "Cancellation" });
    expect(request("o1", "Return", picked({ a: 1 }), false).lines).toEqual([{ sales_order_line: "a", qty: "1", restock: false }]);
  });

  it("puts goods back on the shelf as the person said, else as the reason usually means", () => {
    expect(restocks(emptyChoice(), "a", true)).toBe(true);
    expect(restocks(emptyChoice(), "a", false)).toBe(false);
    expect(restocks({ ...emptyChoice(), restock: { a: true } }, "a", false)).toBe(true);
  });

  it("lets the person go ahead only when the server has an amount for what they chose", () => {
    const ok = readiness({ plan: plan(), kind: "Return", choice: picked({ a: 1 }), reason: "r1", estimating: false });
    expect(ok).toEqual({ ok: true, why: "" });
    expect(readiness({ plan: undefined, kind: "Return", choice: picked({ a: 1 }), reason: "r1", estimating: false }).ok).toBe(false);
    expect(readiness({ plan: plan({ payable: false, why: "Nothing has been paid." }), kind: "Return", choice: picked({ a: 1 }), reason: "r1", estimating: false })).toEqual({ ok: false, why: "Nothing has been paid." });
    expect(readiness({ plan: plan(), kind: "Return", choice: emptyChoice(), reason: "r1", estimating: false }).why).toBe("Choose what comes back.");
    expect(readiness({ plan: plan(), kind: "Return", choice: picked({ a: 1 }), reason: null, estimating: false }).why).toBe("Say why.");
    expect(readiness({ plan: plan(), kind: "Return", choice: picked({ a: 1 }), reason: "r1", estimating: true }).ok).toBe(false);
    expect(readiness({ plan: plan({ estimate: null }), kind: "Return", choice: picked({ a: 1 }), reason: "r1", estimating: false }).ok).toBe(false);
    expect(readiness({ plan: plan({ estimate: { amount: "0", errors: ["at most 1 of Blue Mug can still come back"] } }), kind: "Return", choice: picked({ a: 3 }), reason: "r1", estimating: false }).why).toBe("at most 1 of Blue Mug can still come back");
    expect(readiness({ plan: plan({ estimate: { amount: "0", errors: [] } }), kind: "Return", choice: picked({ a: 1 }), reason: "r1", estimating: false }).why).toBe("There is nothing to send back.");
  });

  it("offers a cancellation only to an order that has not shipped", () => {
    const stale = readiness({ plan: plan(), kind: "Cancellation", choice: emptyChoice(), reason: "r1", estimating: false });
    expect(stale.ok).toBe(false);
    expect(stale.why).toMatch(/shipped/);
    expect(readiness({ plan: plan({ cancel: { allowed: true } }), kind: "Cancellation", choice: emptyChoice(), reason: "r1", estimating: false }).ok).toBe(true);
  });
});
