import { describe, expect, it } from "vitest";
import { exceptionBucket, exceptionWords, gatewayName, refundBucket, refundWords, stuck, waiting } from "./standing";

const money = (v: unknown, code?: string | null) => `${code ?? ""} ${v}`.trim();
const now = new Date("2026-11-01T12:00:00Z");

describe("money that needs a person", () => {
  it("puts a refund where it waits", () => {
    expect(refundBucket({ id: "1", status: "Requested" })).toBe("decide");
    expect(refundBucket({ id: "1", status: "Refund Failed" })).toBe("decide");
    expect(refundBucket({ id: "1", status: "Approved" })).toBe("waiting");
    expect(refundBucket({ id: "1", status: "Refunded", booked: false })).toBe("decide");
    expect(refundBucket({ id: "1", status: "Refunded", booked: true })).toBe("done");
    expect(refundBucket({ id: "1", status: "Declined" })).toBe("done");
  });

  it("puts money a gateway could not book where it waits", () => {
    expect(exceptionBucket({ id: "1", status: "Open" })).toBe("decide");
    expect(exceptionBucket({ id: "1", status: "Refund Failed" })).toBe("decide");
    expect(exceptionBucket({ id: "1", status: "Refund Approved" })).toBe("waiting");
    expect(exceptionBucket({ id: "1", status: "Refunded" })).toBe("done");
    expect(exceptionBucket({ id: "1", status: "Resolved" })).toBe("done");
  });

  it("says a refund's state in words", () => {
    const asked = refundWords({ id: "1", status: "Requested", requested_by: "asha@shop.test", amount: "354", currency_code: "INR", kind: "Return" }, money, now);
    expect(asked).toMatchObject({ headline: "Waiting for a decision", tone: "amber" });
    expect(asked.detail).toBe("asha@shop.test asked to send INR 354 back (goods coming back).");
    const sending = refundWords({ id: "1", status: "Approved", gateway: "stripe", amount: "354", approved_at: "2026-11-01T11:55:00Z" }, money, now);
    expect(sending).toMatchObject({ headline: "Approved: Stripe is sending it", tone: "blue" });
    const late = refundWords({ id: "1", status: "Approved", gateway: "stripe", amount: "354", approved_at: "2026-11-01T11:00:00Z" }, money, now);
    expect(late).toMatchObject({ headline: "Approved: Stripe has not sent it", tone: "amber" });
    const cash = refundWords({ id: "1", status: "Approved", gateway: "Cash on Delivery", approved_at: "2026-11-01T11:59:00Z" }, money, now);
    expect(cash.headline).toBe("Approved: waiting for you");
    expect(refundWords({ id: "1", status: "Refund Failed", gateway: "razorpay", refund_error: "card closed" }, money, now)).toMatchObject({ headline: "Razorpay could not send it", detail: "card closed", tone: "red" });
    expect(refundWords({ id: "1", status: "Refunded", booked: false, booking_error: "The period is closed." }, money, now)).toMatchObject({ headline: "Sent back, but not in the books yet", detail: "The period is closed.", tone: "red" });
    expect(refundWords({ id: "1", status: "Refunded", booked: true, amount: "354", currency_code: "INR", refund_reference: "re_1" }, money, now)).toMatchObject({ headline: "Sent back", detail: "INR 354 went back (re_1).", tone: "green" });
    expect(refundWords({ id: "1", status: "Declined", decision_note: "Not eligible" }, money, now)).toMatchObject({ headline: "Declined", detail: "Not eligible" });
  });

  it("says money a gateway could not book in words", () => {
    const open = exceptionWords({ id: "1", status: "Open", gateway: "paypal", amount: "1149.88", currency_code: "INR", reason: "It was paid after the request closed." }, money, now);
    expect(open).toMatchObject({ headline: "Taken, but could not be booked", tone: "red" });
    expect(open.detail).toBe("INR 1149.88 reached Paypal. It was paid after the request closed.");
    expect(exceptionWords({ id: "1", status: "Open", gateway: "stripe", reason: "x" }, money, now).detail).toBe("Money reached Stripe. x");
    expect(exceptionWords({ id: "1", status: "Resolved", note: "Refunded on the dashboard" }, money, now)).toMatchObject({ headline: "Settled another way", detail: "Refunded on the dashboard" });
    expect(exceptionWords({ id: "1", status: "Refund Approved", gateway: "stripe", approved_at: "2026-11-01T10:00:00Z" }, money, now).tone).toBe("amber");
  });

  it("notices a refund the gateway has not sent", () => {
    expect(stuck("2026-11-01T11:50:00Z", now)).toBe(false);
    expect(stuck("2026-11-01T11:30:00Z", now)).toBe(true);
    expect(stuck(null, now)).toBe(false);
    expect(stuck("nonsense", now)).toBe(false);
  });

  it("counts what waits for a person", () => {
    const refunds = [{ id: "1", status: "Requested" }, { id: "2", status: "Approved" }, { id: "3", status: "Refunded", booked: false }, { id: "4", status: "Refunded", booked: true }];
    const exceptions = [{ id: "1", status: "Open" }, { id: "2", status: "Resolved" }];
    expect(waiting(refunds, exceptions)).toBe(3);
    expect(waiting([], [])).toBe(0);
    expect(gatewayName(null)).toBe("the gateway");
  });
});
