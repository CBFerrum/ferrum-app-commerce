// Badge colours for the states this app shows, keyed by the stored state id (never the words on
// screen, which are translated). An id not listed is neutral.
import type { Tone } from "@ferrum/kit";

type Tones = Readonly<Record<string, Tone>>;

/** SalesOrder.commerce_payment_status */
export const ORDER_PAYMENT: Tones = { Pending: "amber", Paid: "green", Failed: "red", Expired: "amber", Cancelled: "neutral", "Not Required": "neutral", "Partly Refunded": "amber", Refunded: "neutral" };

/** PaymentRequest.status */
export const REQUEST_PAYMENT: Tones = { Initiated: "blue", Paid: "green", Failed: "red", Cancelled: "neutral", Expired: "amber" };

/** SalesOrder.status */
export const ORDER_STATUS: Tones = { Draft: "neutral", "To Deliver and Bill": "blue", "To Deliver": "blue", "To Bill": "blue", Completed: "green", Closed: "neutral", Cancelled: "red" };

/** SalesInvoice.status */
export const INVOICE_STATUS: Tones = { Draft: "neutral", Unpaid: "amber", "Partly Paid": "amber", Paid: "green", Credit: "neutral", Return: "neutral", Cancelled: "red" };
