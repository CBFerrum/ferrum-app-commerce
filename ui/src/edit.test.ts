import { describe, expect, it } from "vitest";
import { addressParam, addressProblem, cancelParams, changes, emptyAddress, emptyDraft, estimate, type OrderLine, parts, partWords, readiness, savedWords, settlement, updateParams } from "./edit";

const lines: OrderLine[] = [
  { id: "a", item: "mug", item_name: "Blue Mug", qty: "2", rate: "250", amount: "500", delivered_qty: "0" },
  { id: "b", item: "tee", item_name: "Logo Tee", qty: "1", rate: "400", amount: "400", delivered_qty: "1" },
  { id: "c", item: "ship", item_name: "Shipping", qty: "1", rate: "50", amount: "50", delivered_qty: "0" },
];
const draft = (over = {}) => ({ ...emptyDraft(), ...over });

describe("what is asked of the server", () => {
  it("names a line by its number on the order, and an item added by its id", () => {
    const d = draft({ qty: { a: "3", b: "1", c: "5" }, added: [{ key: 1, item: "item-9", title: "Cap", qty: "2" }, { key: 2, item: null, title: "", qty: "1" }] });
    // b is unchanged and the unchosen added row says nothing.
    expect(changes(lines, d)).toEqual([{ line: 1, qty: "3" }, { line: 3, qty: "5" }, { item: "item-9", qty: "2" }]);
    expect(changes(lines, draft({ qty: { a: "0" } }))).toEqual([{ line: 1, qty: "0" }]);
    expect(updateParams(lines, draft({ qty: { a: "0" } }), " why ", false, true)).toEqual({ changes: '[{"line":1,"qty":"0"}]', address: undefined, note: "why", notify: "No", approve: "Yes" });
  });

  it("sends the address only when there is one, as the server reads it", () => {
    expect(addressParam(null)).toBeUndefined();
    expect(addressParam({ ...emptyAddress(), line1: " 1 Road ", city: "Pune", country: "in" })).toBe('{"line1":"1 Road","city":"Pune","country":"IN"}');
    expect(addressProblem({ ...emptyAddress(), line1: "1 Road", city: "Pune" })).toMatch(/street, a city and a country/);
    expect(addressProblem({ ...emptyAddress(), line1: "1 Road", city: "Pune", country: "India" })).toMatch(/two letters/);
    expect(addressProblem({ ...emptyAddress(), line1: "1 Road", city: "Pune", country: "IN" })).toBeNull();
  });

  it("will not save nothing, or a quantity ERP would refuse", () => {
    const charge = (l: OrderLine) => l.item === "ship";
    expect(readiness(lines, draft(), charge).ok).toBe(false);
    expect(readiness(lines, draft({ qty: { b: "0" } }), charge).why).toMatch(/1 of this line have shipped/);
    expect(readiness(lines, draft({ qty: { a: "x" } }), charge).why).toMatch(/number of 0 or more/);
    expect(readiness(lines, draft({ added: [{ key: 1, item: null, title: "", qty: "1" }] }), charge).why).toMatch(/Choose the item/);
    expect(readiness(lines, draft({ qty: { a: "1" } }), charge)).toEqual({ ok: true, why: "" });
    expect(readiness(lines, draft({ address: { ...emptyAddress(), line1: "1 Road", city: "Pune", country: "IN" } }), charge).ok).toBe(true);
  });
});

describe("the difference before saving", () => {
  it("prices what the desk can price, and says what it cannot", () => {
    expect(estimate(lines, draft({ qty: { a: "1" } }))).toEqual({ goods: -250, added: 0, moves: false });
    expect(estimate(lines, draft({ qty: { a: "3" }, added: [{ key: 1, item: "x", title: "", qty: "2" }], address: emptyAddress() }))).toEqual({ goods: 250, added: 1, moves: true });
  });

  it("follows the server: replace an open request, ask for more, send some back, or record it as owed", () => {
    const stripe = { id: "p", gateway: "stripe", amount: "1000", status: "Paid", refunded_amount: "0" };
    expect(settlement(-250, [{ id: "p", gateway: "stripe", amount: "1000", status: "Initiated" }])).toEqual({ kind: "replace", via: "stripe" });
    expect(settlement(0, [{ id: "p", gateway: "stripe", status: "Initiated" }])).toEqual({ kind: "none" });
    expect(settlement(100, [stripe])).toEqual({ kind: "charge", via: "stripe" });
    expect(settlement(-250, [stripe])).toEqual({ kind: "refund", via: "stripe", offline: false });
    // The gateway's payment has been refunded already: it cannot take this one, so it is owed.
    expect(settlement(-250, [{ ...stripe, refunded_amount: "900" }])).toEqual({ kind: "owed", toShopper: true });
    // A gift card's part cannot take a refund through a gateway, nor be asked for again.
    expect(settlement(-50, [{ id: "g", gateway: "Gift Card", amount: "100", status: "Paid" }])).toEqual({ kind: "owed", toShopper: true });
    expect(settlement(-50, [{ id: "p", gateway: "Cash on Delivery", amount: "300", status: "Paid" }])).toEqual({ kind: "refund", via: "Cash on Delivery", offline: true });
    expect(settlement(80, [])).toEqual({ kind: "owed", toShopper: false });
  });

  it("tells what was saved from the server's own figures", () => {
    const money = (v: unknown) => `₹${Number(v)}`;
    expect(savedWords({ total: "1100", difference: "100", owed: "0", payment_request: "pr1" }, money)).toBe("Saved. The order now comes to ₹1100. That is ₹100 more than before. The shopper is asked for it with a payment link on their order page.");
    expect(savedWords({ total: "900", difference: "-100", owed: "-100", refund: null }, money)).toContain("The store owes the shopper ₹100.");
  });
});

describe("cancelling", () => {
  const paid = [
    { id: "s", gateway: "stripe", amount: "300", status: "Paid" },
    { id: "g", gateway: "Gift Card", amount: "150", status: "Paid" },
    { id: "x", gateway: "stripe", amount: "300", status: "Expired" },
  ];
  it("sends the gift card's part back first, then each provider's, and nothing for what was never paid", () => {
    const p = parts(paid);
    expect(p.map((x) => [x.gateway, x.amount, x.card])).toEqual([["Gift Card", 150, true], ["stripe", 300, false]]);
    expect(parts([{ id: "i", gateway: "stripe", amount: "300", status: "Initiated" }])).toEqual([]);
    expect(partWords(p[0], true, "the gift card")).toMatch(/back onto the gift card now/);
    expect(partWords(p[1], false, "Stripe")).toMatch(/waits for a person to approve it/);
    expect(partWords(parts([{ id: "m", gateway: "Manual", amount: "5", status: "Paid" }])[0], true, "Manual")).toMatch(/handed back by you/);
  });

  it("asks the server as Cancel Web Order does", () => {
    expect(cancelParams("", " ", true)).toEqual({ reason: undefined, note: undefined, approve: "Yes" });
    expect(cancelParams("r1", "late", false)).toEqual({ reason: "r1", note: "late", approve: "No" });
  });
});
