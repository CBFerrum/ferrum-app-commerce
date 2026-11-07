// Shipping a web order: the carrier and tracking the shopper will see, whether to tell them, and
// whether to invoice at once. One request does it all or nothing: if the warehouse is short, the
// server says so and nothing is left half done.

import { type FormEvent, useState } from "react";
import { api, docPath, errorMessage, t, isolate } from "@ferrum/client";
import { Alert, Button, Dialog, Field, toast } from "@ferrum/kit";
import { isOffline, type Order } from "./queues";

const CARRIER = "commerce:last-carrier";

/** The carrier used last time, kept in this browser as a convenience only. */
function lastCarrier(): string {
  try {
    return localStorage.getItem(CARRIER) ?? "";
  } catch {
    return "";
  }
}

export function ShipDialog({ order, onClose, onDone }: { order: Pick<Order, "id" | "number" | "commerce_payment_method" | "commerce_payment_status" | "customer_name">; onClose: () => void; onDone: () => void }) {
  const [carrier, setCarrier] = useState(lastCarrier);
  const [number, setNumber] = useState("");
  const [link, setLink] = useState("");
  const [notify, setNotify] = useState(true);
  const [invoice, setInvoice] = useState(true);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const cash = order.commerce_payment_method === "Cash on Delivery" && order.commerce_payment_status !== "Paid";
  const linkError = link.trim() && !/^https:\/\//i.test(link.trim()) ? "Use a link that starts with https://" : undefined;

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    if (linkError) return;
    setBusy(true);
    setError(null);
    try {
      await api.post(docPath("SalesOrder", order.id, "actions", "web_ship"), {
        carrier: carrier.trim() || undefined,
        tracking_number: number.trim() || undefined,
        tracking_url: link.trim() || undefined,
        notify,
        invoice,
      });
      try {
        if (carrier.trim()) localStorage.setItem(CARRIER, carrier.trim());
      } catch {
        // Remembering the carrier is a convenience.
      }
      toast(t("{value} shipped", { value: order.number ?? "Order" }));
      onDone();
    } catch (err) {
      setError(errorMessage(err));
      setBusy(false);
    }
  };

  return (
    <Dialog open onClose={busy ? () => {} : onClose} title={t("Ship {value}", { value: order.number ?? "order" })}>
      <form className="stack" onSubmit={submit}>
        {error && <Alert tone="error">{isolate(error)}</Alert>}
        <p className="muted cm-tight">
          {t("Everything still to deliver on this order leaves the warehouse")}{order.customer_name ? t(" for {customer_name}", { customer_name: order.customer_name }) : ""}.
          {cash && t(" Cash on delivery: the courier collects the money, and you record it when it arrives.")}
          {isOffline(order.commerce_payment_method) && !cash && order.commerce_payment_status !== "Paid" && t(" Its payment is not recorded yet.")}
        </p>
        <Field label={t("Carrier")} htmlFor="ship-carrier" hint={t("Who takes it, for the shopper to see.")}>
          <input id="ship-carrier" className="input" value={carrier} maxLength={140} autoComplete="off" onChange={(e) => setCarrier(e.target.value)} />
        </Field>
        <Field label={t("Tracking number")} htmlFor="ship-number">
          <input id="ship-number" className="input" value={number} maxLength={120} autoComplete="off" onChange={(e) => setNumber(e.target.value)} />
        </Field>
        <Field label={t("Tracking link")} htmlFor="ship-link" error={linkError} hint={t("Where the carrier shows the parcel.")}>
          <input id="ship-link" className="input" type="url" inputMode="url" placeholder="https://" value={link} aria-invalid={!!linkError} onChange={(e) => setLink(e.target.value)} />
        </Field>
        <label className="check-field">
          <input type="checkbox" checked={notify} onChange={(e) => setNotify(e.target.checked)} />
          <span>{t("Tell the shopper it has shipped")}</span>
        </label>
        <label className="check-field">
          <input type="checkbox" checked={invoice} onChange={(e) => setInvoice(e.target.checked)} />
          <span>{t("Create the invoice too (a payment already received is applied to it)")}</span>
        </label>
        <div className="row cm-end-row">
          <Button onClick={onClose} disabled={busy}>
            {t("Cancel")}
          </Button>
          <Button type="submit" variant="primary" icon="check" loading={busy} disabled={!!linkError}>
            {t("Ship")}
          </Button>
        </div>
      </form>
    </Dialog>
  );
}
