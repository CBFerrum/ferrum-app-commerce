import { describe, expect, it } from "vitest";
import { daysBetween, destinationWords, feeWords, groups, planLines, refundSum, shippingWords, stage, windowWords } from "./returnrules";

describe("where a request waits", () => {
  it("sorts by what the desk does next", () => {
    expect(["Requested", "Approved", "Declined", "Received", "Refunded"].map((status) => stage({ status }))).toEqual(["decide", "receive", "done", "done", "done"]);
  });
});

describe("the terms the shopper was given", () => {
  it("says where it goes, who pays the shipping and the fee", () => {
    expect(destinationWords("Store Credit")).toMatch(/store credit/);
    expect(destinationWords("Original Payment")).toMatch(/payment it came from/);
    expect(shippingWords("Store")).toMatch(/store pays/);
    expect(shippingWords("Shopper")).toMatch(/shopper pays/);
    expect(feeWords("10")).toBe("10% restocking fee is kept from what the goods credit");
    expect(feeWords(0)).toBe("No restocking fee");
  });

  it("says whether it was asked inside the window", () => {
    expect(daysBetween("2026-11-01", "2026-11-10T09:00:00Z")).toBe(9);
    expect(daysBetween(null, "2026-11-10")).toBeNull();
    expect(windowWords("2026-11-01", "2026-11-10T09:00:00Z", 14)).toEqual({ words: "Asked 9 days after delivery, inside the store's 14 days.", inside: true });
    expect(windowWords("2026-11-01", "2026-11-20T09:00:00Z", 14).inside).toBe(false);
    expect(windowWords(null, "2026-11-20", 14)).toEqual({ words: "The store takes returns for 14 days after delivery.", inside: null });
    expect(windowWords("2026-11-01", "2026-11-03", 0).inside).toBeNull();
  });
});

describe("the refund before it is confirmed", () => {
  it("takes the restocking fee off what the goods credit", () => {
    expect(refundSum(500, "10")).toEqual({ credit: 500, fee: 50, net: 450 });
    expect(refundSum(333.33, 15)).toEqual({ credit: 333.33, fee: 50, net: 283.33 });
    expect(refundSum(100, null)).toEqual({ credit: 100, fee: 0, net: 100 });
    expect(refundSum(100, 250).net).toBe(0);
  });

  it("asks the plan for the lines as the request holds them", () => {
    expect(planLines([{ sales_order_line: "l1", qty: "2", item_name: "Mug" }])).toEqual([{ sales_order_line: "l1", qty: "2", restock: true }]);
  });
});

describe("stock alerts", () => {
  it("groups by item, most waiting first, and keeps the oldest wait", () => {
    const a = (id: string, item: string, status: string, at: string) => ({ id, website_item: item, item_title: item === "w1" ? "Blue Mug" : "Logo Tee", email: `${id}@x.test`, status, created_at: at });
    const g = groups([a("1", "w2", "Waiting", "2026-11-02"), a("2", "w1", "Waiting", "2026-11-03"), a("3", "w1", "Waiting", "2026-11-01"), a("4", "w1", "Sent", "2026-10-01")]);
    expect(g.map((x) => [x.title, x.waiting.length, x.sent.length, x.oldest])).toEqual([["Blue Mug", 2, 1, "2026-11-01"], ["Logo Tee", 1, 0, "2026-11-02"]]);
  });
});
