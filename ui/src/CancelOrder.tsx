// Cancelling an order that has not shipped: what goes back, payment by payment (a gift card's part
// onto the card first, each provider's through its own gateway), and what happens to each, before
// anything is done. The server makes one refund for each payment and cancels the order when the last
// has gone back; its own words are shown if it refuses.

import { useQuery } from "@tanstack/react-query";
import { useState } from "react";
import { api, docPath, errorMessage, isolate, t } from "@ferrum/client";
import { Alert, Button, Dialog, Field, toast } from "@ferrum/kit";
import { cancelParams, type Part, partWords, parts, type PayRow } from "./edit";
import { f, list, type Rights } from "./data";
import { amountText } from "./queues";
import { gatewayName } from "./standing";

interface Reason {
  id: string;
  reason: string;
}

interface Result {
  cancelled?: boolean;
  refunds?: { id: string; gateway?: string | null; status?: string | null; amount?: string }[];
}

export function CancelOrderDialog({ order, payments, rights, owed, code, onClose, onDone }: { order: { id: string; number?: string | null }; payments: PayRow[]; rights: Rights; owed?: number; code?: string | null; onClose: () => void; onDone: () => void }) {
  const [reasonId, setReasonId] = useState("");
  const [note, setNote] = useState("");
  const [now, setNow] = useState(true);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const reasons = useQuery({ queryKey: ["commerce", "reasons"], queryFn: () => list<Reason>("ReturnReason", { filters: f([["disabled", "=", false]]), fields: "reason", sort: "reason asc", limit: 100 }).then((r) => r.rows) });
  const rows = parts(payments);
  const money = (v: unknown) => amountText(v, code ?? payments[0]?.currency_code);
  const label = (p: Part) => (p.card ? t("the gift card") : gatewayName(p.gateway));
  const total = rows.reduce((n, p) => n + p.amount, 0);
  // Unpaid requests are cancelled with the order and a card's hold is let go: nothing goes back.
  const unpaid = rows.length === 0;
  // Whoever may approve refunds sends them now; anyone else asks, and a manager decides.
  const mayApprove = rights.decides;

  const submit = async () => {
    setBusy(true);
    setError(null);
    try {
      const r = await api.post<Result>(docPath("SalesOrder", order.id, "actions", "web_cancel"), cancelParams(reasonId, note, now && mayApprove));
      const waiting = (r.refunds ?? []).filter((x) => x.status !== "Refunded").length;
      toast(r.cancelled ? t("Cancelled.") : waiting ? t("Asked for. The order is cancelled when the last refund has gone back.") : t("Done."));
      onDone();
    } catch (err) {
      setError(errorMessage(err));
      setBusy(false);
    }
  };

  return (
    <Dialog
      open
      onClose={busy ? () => {} : onClose}
      title={t("Cancel {value}", { value: order.number ?? t("order") })}
      footer={
        <>
          <Button onClick={onClose} disabled={busy}>
            {t("Keep the order")}
          </Button>
          <Button variant="danger" loading={busy} onClick={() => void submit()}>
            {unpaid ? t("Cancel the order") : t("Cancel and send {amount} back", { amount: money(total) })}
          </Button>
        </>
      }
    >
      <div className="stack">
        {error && <Alert tone="error">{isolate(error)}</Alert>}
        {unpaid ? (
          <Alert tone="info">{t("Nothing has been paid on this order. It is cancelled at once, its open payment requests are cancelled with it and any value held on a gift card is let go.")}</Alert>
        ) : (
          <>
            <p className="cm-tight">{t("What was paid goes back payment by payment. The order is cancelled when the last one has gone back.")}</p>
            <table className="table cm-table">
              <thead>
                <tr>
                  <th>{t("Paid with")}</th>
                  <th className="num">{t("Goes back")}</th>
                  <th>{t("What happens")}</th>
                </tr>
              </thead>
              <tbody>
                {rows.map((p) => (
                  <tr key={p.id}>
                    <td data-label={t("Paid with")}>{isolate(p.card ? t("Gift card") : gatewayName(p.gateway))}</td>
                    <td className="num" data-label={t("Goes back")}>
                      {isolate(money(p.amount))}
                    </td>
                    <td data-label={t("What happens")}>{isolate(partWords(p, now && mayApprove, label(p)))}</td>
                  </tr>
                ))}
              </tbody>
              <tfoot>
                <tr className="bold">
                  <td>{t("In all")}</td>
                  <td className="num">{isolate(money(total))}</td>
                  <td />
                </tr>
              </tfoot>
            </table>
            {mayApprove ? (
              <label className="check-field">
                <input type="checkbox" checked={now} onChange={(e) => setNow(e.target.checked)} />
                <span>{t("Send the money back now (otherwise each refund waits for a person to approve it)")}</span>
              </label>
            ) : (
              <p className="faint cm-tight">{t("You ask for these refunds and a manager approves them.")}</p>
            )}
          </>
        )}
        {!!owed && (
          <Alert tone="warning">
            {owed > 0 ? t("An earlier change left {amount} owed by the shopper: it is not collected by cancelling.", { amount: money(owed) }) : t("An earlier change left {amount} owed to the shopper: settle it with them.", { amount: money(-owed) })}
          </Alert>
        )}
        <Field label={t("Why")} htmlFor="cancel-reason" hint={t("Left empty, it is recorded as cancelled before it shipped.")}>
          <select id="cancel-reason" className="select" value={reasonId} onChange={(e) => setReasonId(e.target.value)}>
            <option value="">{t("Cancelled before it shipped")}</option>
            {reasons.data?.map((r) => (
              <option key={r.id} value={r.id}>
                {isolate(r.reason)}
              </option>
            ))}
          </select>
        </Field>
        <Field label={t("Note")} htmlFor="cancel-note">
          <textarea id="cancel-note" className="textarea" rows={2} maxLength={2000} value={note} onChange={(e) => setNote(e.target.value)} />
        </Field>
      </div>
    </Dialog>
  );
}
