import { describe, expect, it } from "vitest";
import { addressParam, addressProblem, cancelParams, changes, emptyAddress, emptyDraft, type OrderLine, parts, partWords, previewBody, readiness, savedWords, settlementWords, updateParams } from "./edit";

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

describe("what the server says saving would do", () => {
  const money = (v: unknown) => `₹${Number(v)}`;
  const via = (g?: string | null, l?: string | null) => l ?? g ?? "the first method";

  it("asks as the save does, for this order and never to approve a refund", () => {
    expect(previewBody("o1", lines, draft({ qty: { a: "1" } }), "")).toEqual({ sales_order: "o1", changes: '[{"line":1,"qty":"1"}]', address: undefined, note: undefined, notify: "No", approve: "No", shipping_rule: undefined });
    expect(previewBody("o1", lines, draft({ qty: { a: "1" } }), "r9").shipping_rule).toBe("r9");
  });

  it("puts each settlement the server names in words: charge, refund, owed, none", () => {
    expect(settlementWords({ kind: "none" }, money, via).text).toBe("Nothing is asked for and nothing is sent back.");
    expect(settlementWords({ kind: "charge", amount: "100", label: "Card", replaces_open_request: true }, money, via).text).toBe("The shopper has not paid yet: their open payment request is replaced by one for ₹100, through Card.");
    expect(settlementWords({ kind: "charge", amount: "100", label: "Card", replaces_open_request: false }, money, via).text).toMatch(/is asked for ₹100 through Card, with a payment link/);
    expect(settlementWords({ kind: "refund", amount: "250", gateway: "stripe", status: "Requested" }, money, via)).toEqual({ text: "₹250 is sent back through stripe as an adjustment refund.", approvable: true });
    expect(settlementWords({ kind: "refund", amount: "50", gateway: "Cash on Delivery" }, money, via)).toMatchObject({ approvable: false, text: expect.stringContaining("you hand it back") });
    expect(settlementWords({ kind: "owed", amount: "80", by: "shopper" }, money, via).text).toMatch(/owed by the shopper: ₹80 is still to collect/);
    expect(settlementWords({ kind: "owed", amount: "80", by: "store" }, money, via).text).toMatch(/the store owes ₹80/);
  });

  it("tells what was saved from the server's own figures", () => {
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
