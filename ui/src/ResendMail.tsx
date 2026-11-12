// Send Mail Again: tells the shopper once more, as the order stands now, one of the mails whose words come from
// the order itself (src/resend.rs). It goes to the order's own address and no other, is counted on the order and
// limited per mail and day (Store Settings); a mail with a secret value is not among them, and the server's
// refusal is shown as it is.

import { useState } from "react";
import { api, docPath, errorMessage, isolate, t } from "@ferrum/client";
import { Alert, Button, Dialog, Field, toast } from "@ferrum/kit";

/** The mails that may be sent again, by the label the action takes, with what must exist on the order for there to be anything to tell. */
export type MailKind = "Shipped" | "Payment Received" | "Payment Failed" | "Refunded";
export interface Available {
  Shipped: boolean;
  "Payment Received": boolean;
  "Payment Failed": boolean;
  Refunded: boolean;
}

/** What the server answers (src/resend.rs): whether it went, the times in all and what is left of today's. */
export interface Resent {
  sent?: boolean;
  state?: string | null;
  mail?: string;
  resent?: string;
  left_today?: number;
}

export const mailWords = (): Record<MailKind, { label: string; hint: string; none: string }> => ({
  Shipped: { label: t("Shipped"), hint: t("What shipped, the carrier and the tracking."), none: t("Nothing on this order has shipped.") },
  "Payment Received": { label: t("Payment received"), hint: t("The amount and the method of the payment that came in."), none: t("No payment has come in on this order.") },
  "Payment Failed": { label: t("Payment failed"), hint: t("Why the payment did not go through, with the link to try again."), none: t("No payment on this order has failed.") },
  Refunded: { label: t("Refunded"), hint: t("The amount sent back."), none: t("Nothing has been sent back on this order yet.") },
});

/** What the desk is told once it is sent. */
export function resentWords(r: Resent): { text: string; ok: boolean } {
  if (!r.sent) return { ok: false, text: t("The mail was made but could not be sent ({state}). Look in the order's story, or try again later.", { state: r.state ?? t("unknown") }) };
  const left = r.left_today ?? 0;
  return { ok: true, text: left > 0 ? t("Sent again. This mail may go to this order {left} more times today.", { left }) : t("Sent again. This mail has reached its limit for this order today.") };
}

export function ResendMailDialog({ order, available, email, limit, onClose, onDone }: { order: { id: string; number?: string | null; commerce_mails_resent?: string | number | null }; available: Available; email?: string | null; limit?: number | null; onClose: () => void; onDone: () => void }) {
  const words = mailWords();
  const kinds = Object.keys(words) as MailKind[];
  const first = kinds.find((k) => available[k]) ?? "";
  const [mail, setMail] = useState<string>(first);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const times = Number(order.commerce_mails_resent ?? 0);

  const submit = async () => {
    if (!mail || busy) return;
    setBusy(true);
    setError(null);
    try {
      const r = await api.post<Resent>(docPath("SalesOrder", order.id, "actions", "web_resend_mail"), { mail });
      const said = resentWords(r);
      toast(said.text, said.ok ? undefined : "error");
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
      title={t("Send a mail again · {value}", { value: order.number ?? t("order") })}
      footer={
        <>
          <Button onClick={onClose} disabled={busy}>
            {t("Close")}
          </Button>
          <Button variant="primary" icon="send" loading={busy} disabled={!mail || !email} onClick={() => void submit()}>
            {t("Send it again")}
          </Button>
        </>
      }
    >
      <div className="stack">
        {error && <Alert tone="error">{isolate(error)}</Alert>}
        {email ? <p className="cm-tight">{t("The shopper is told again as the order stands now, at {email} and nowhere else.", { email })}</p> : <Alert tone="warning">{t("This order has no email address to write to.")}</Alert>}
        <Field label={t("Which mail")} htmlFor="resend-mail">
          <fieldset className="stack" id="resend-mail">
            {kinds.map((k) => (
              <label key={k} className="choice">
                <input type="radio" name="resend" value={k} checked={mail === k} disabled={!available[k]} onChange={() => setMail(k)} />
                <span>
                  <strong>{words[k].label}</strong>
                  <div className="muted">{available[k] ? words[k].hint : words[k].none}</div>
                </span>
              </label>
            ))}
          </fieldset>
        </Field>
        <p className="faint cm-tight">{t("The confirmation of the order and a gift card's or store credit's code cannot be sent again: they hold a secret value that is not kept.")}</p>
        <p className="faint cm-tight">
          {times > 0 ? t("Mails have been sent again {count} times on this order.", { count: times }) : t("No mail has been sent again on this order yet.")}{" "}
          {limit ? t("The same mail goes to one order at most {limit} times a day, the first send included.", { limit }) : ""}
        </p>
      </div>
    </Dialog>
  );
}
