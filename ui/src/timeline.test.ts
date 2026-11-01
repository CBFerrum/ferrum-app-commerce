import { describe, expect, it } from "vitest";
import { fold, timeline } from "./timeline";

const say = { money: (v: unknown, code?: string | null) => `${code ?? ""} ${v}`.trim(), gateway: (g?: string | null) => g ?? "the gateway" };
const empty = { payments: [], deliveries: [], invoices: [], refunds: [], exceptions: [], audit: [] };

describe("an order's story", () => {
  it("tells the order, its payment, its shipment and its invoice, newest first", () => {
    const told = timeline(
      {
        ...empty,
        order: { created_at: "2026-11-01T09:00:00.000Z", customer_name: "Asha Rao", rounded_total: "1121", currency_code: "INR" },
        payments: [{ id: "p", gateway: "stripe", amount: "1121", currency_code: "INR", status: "Paid", created_at: "2026-11-01T09:00:05.000Z", paid_at: "2026-11-01T09:02:00.000Z", gateway_reference: "pi_1" }],
        deliveries: [{ id: "n", number: "DN-1", created_at: "2026-11-01T10:00:00.000Z", transporter: "BlueDart", commerce_tracking_number: "BD123", commerce_tracking_url: "https://t.example/BD123" }],
        invoices: [{ id: "i", number: "SINV-1", created_at: "2026-11-01T10:00:02.000Z", rounded_total: "1121" }],
      },
      say,
    );
    expect(told.map((e) => e.title)).toEqual(["Invoice SINV-1", "Shipped", "Paid via stripe", "Payment of INR 1121 asked via stripe", "Order placed"]);
    expect(told[1]).toMatchObject({ detail: "BlueDart · BD123", to: "https://t.example/BD123" });
    expect(told[2].detail).toBe("pi_1");
    expect(told[4].detail).toBe("Asha Rao · INR 1121");
  });

  it("tells what became of a payment that was not made", () => {
    const failed = timeline({ ...empty, order: { created_at: "2026-11-01T09:00:00.000Z" }, payments: [{ id: "p", gateway: "razorpay", amount: "10", status: "Failed", created_at: "2026-11-01T09:00:05.000Z", updated_at: "2026-11-01T09:05:00.000Z", failure_reason: "card declined" }] }, say);
    expect(failed[0]).toMatchObject({ title: "Payment failed", detail: "card declined" });
    const expired = timeline({ ...empty, order: { created_at: "2026-11-01T09:00:00.000Z" }, payments: [{ id: "p", status: "Expired", created_at: "2026-11-01T09:00:05.000Z", expires_at: "2026-11-01T09:30:00.000Z" }] }, say);
    expect(expired[0].title).toBe("Payment ran out of time");
  });

  it("tells a refund from the asking to the sending", () => {
    const told = timeline(
      {
        ...empty,
        order: { created_at: "2026-11-01T09:00:00.000Z" },
        refunds: [{ id: "r", kind: "Return", amount: "354", currency_code: "INR", status: "Refunded", requested_by: "a@shop.test", approved_by: "m@shop.test", created_at: "2026-11-02T09:00:00.000Z", approved_at: "2026-11-02T09:10:00.000Z", refunded_at: "2026-11-02T09:15:00.000Z" }],
        deliveries: [{ id: "n", number: "DN-2", is_return: true, created_at: "2026-11-02T09:15:01.000Z" }],
      },
      say,
    );
    expect(told.map((e) => e.title)).toEqual(["Goods back in stock", "INR 354 sent back", "Refund approved", "Refund of INR 354 asked for", "Order placed"]);
    expect(told[2].who).toBe("m@shop.test");
    const failed = timeline({ ...empty, order: { created_at: "2026-11-01T09:00:00.000Z" }, refunds: [{ id: "r", status: "Refund Failed", refund_error: "card closed", created_at: "2026-11-02T09:00:00.000Z", approved_at: "2026-11-02T09:10:00.000Z", updated_at: "2026-11-02T09:20:00.000Z" }] }, say);
    expect(failed[0]).toMatchObject({ title: "The gateway could not send the refund", detail: "card closed" });
    const cancelled = timeline({ ...empty, order: { posting_date: "2026-11-01" }, refunds: [{ id: "r", kind: "Cancellation", amount: "300", created_at: "2026-11-02T09:00:00.000Z" }] }, say);
    expect(cancelled[0].title).toMatch(/order cancelled/);
  });

  it("tells money that could not be booked, and the order being cancelled or closed, and nothing else the audit trail holds", () => {
    const told = timeline(
      {
        ...empty,
        order: { created_at: "2026-11-01T09:00:00.000Z" },
        exceptions: [{ id: "x", amount: "10", currency_code: "INR", gateway: "paypal", reason: "paid after it closed", created_at: "2026-11-01T11:00:00.000Z" }],
        audit: [
          { id: 1, at: "2026-11-01T09:00:00.000Z", action: "create" },
          { id: 2, at: "2026-11-01T12:00:00.000Z", action: "cancel", actor_name: "Asha" },
          { id: 3, at: "2026-11-01T13:00:00.000Z", action: "action:close", actor: "m@shop.test" },
          { id: 4, at: "2026-11-01T13:30:00.000Z", action: "update" },
        ],
      },
      say,
    );
    expect(told.map((e) => e.title)).toEqual(["Order closed", "Order cancelled", "INR 10 reached paypal and could not be booked", "Order placed"]);
    expect(told[1].who).toBe("Asha");
  });

  it("puts a dated record among the day's timed ones, and drops what has no time", () => {
    const told = timeline({ ...empty, order: { posting_date: "2026-11-01" }, invoices: [{ id: "i", number: "S1", posting_date: "2026-11-01" }, { id: "j", number: "S2" }] }, say);
    expect(told.length).toBe(2);
    expect(told[0].at).toBe("2026-11-01T12:00:00.000Z");
  });

  it("folds the old", () => {
    const many = Array.from({ length: 12 }, (_, n) => ({ key: String(n), at: `2026-11-01T10:${String(n).padStart(2, "0")}:00.000Z`, kind: "order" as const, title: String(n) }));
    expect(fold(many, 8)).toMatchObject({ hidden: 4 });
    expect(fold(many, 8).shown.length).toBe(8);
    expect(fold(many.slice(0, 3), 8)).toMatchObject({ hidden: 0 });
  });
});
