// Sending money back for a web order: choose the lines that come back and how many, whether they go
// back on the shelf, and why; or cancel an order that has not shipped. The server says how many of
// each line may still come back and what the choice comes to (the same credit note it will make), so
// the total moves with the steppers and no rule lives here. A manager sends it from here; anyone else
// asks, and a manager decides.

import { keepPreviousData, useQuery } from "@tanstack/react-query";
import { useEffect, useState } from "react";
import { api, docPath, errorMessage, t, isolate } from "@ferrum/client";
import { Alert, Button, confirmDialog, Dialog, Field, Spinner, toast, useDebounced } from "@ferrum/kit";
import { amountText, units } from "./queues";
import { f, list, type Rights } from "./data";
import { gatewayName } from "./standing";
import { type Choice, clampQty, emptyChoice, type Kind, type Plan, type PlanLine, readiness, request, restocks } from "./choices";

interface Reason {
  id: string;
  reason: string;
  restock?: boolean | null;
}

function Stepper({ line, value, onChange }: { line: PlanLine; value: number; onChange: (n: number) => void }) {
  const max = Number(line.returnable);
  const name = line.name ?? "item";
  return (
    <div className="cm-stepper" role="group" aria-label={t("How many {name} come back", { name })}>
      <button type="button" className="btn btn-default btn-sm" aria-label={t("One fewer {name}", { name })} disabled={value <= 0} onClick={() => onChange(clampQty(value - 1, max))}>
        −
      </button>
      <input
        className="input num"
        type="number"
        inputMode="decimal"
        min={0}
        max={max}
        step="any"
        aria-label={t("Number of {name} to send back", { name })}
        value={value || ""}
        placeholder="0"
        onChange={(e) => onChange(clampQty(e.target.value, max))}
      />
      <button type="button" className="btn btn-default btn-sm" aria-label={t("One more {name}", { name })} disabled={value >= max} onClick={() => onChange(clampQty(value + 1, max))}>
        +
      </button>
    </div>
  );
}

export function RefundDialog({ order, rights, onClose, onDone }: { order: { id: string; number?: string | null; customer_name?: string | null }; rights: Rights; onClose: () => void; onDone: () => void }) {
  const [kind, setKind] = useState<Kind | null>(null);
  const [choice, setChoice] = useState<Choice>(emptyChoice);
  const [reasonId, setReasonId] = useState<string>("");
  const [note, setNote] = useState("");
  const [notify, setNotify] = useState(true);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const reasons = useQuery({ queryKey: ["commerce", "reasons"], queryFn: () => list<Reason>("ReturnReason", { filters: f([["disabled", "=", false]]), fields: "reason,restock", sort: "reason asc", limit: 100 }).then((r) => r.rows) });
  const reason = reasons.data?.find((r) => r.id === reasonId);
  const reasonRestocks = reason ? reason.restock !== false : true;
  const asked = request(order.id, kind ?? "Return", choice, reasonRestocks);
  const key = useDebounced(JSON.stringify(asked), 250);
  const plan = useQuery({
    queryKey: ["commerce", "refund-plan", key],
    queryFn: ({ signal }) => api.post<Plan>("/api/app/commerce/desk/refund-plan", JSON.parse(key) as unknown, { signal }),
    placeholderData: keepPreviousData,
    gcTime: 0,
  });
  const p = plan.data;
  const estimating = plan.isFetching || key !== JSON.stringify(asked);

  // An order that has not shipped can only be cancelled; one that has, can only have goods come back.
  useEffect(() => {
    if (kind === null && p?.payable) setKind(p.cancel?.allowed ? "Cancellation" : "Return");
  }, [kind, p]);

  const current: Kind = kind ?? "Return";
  const ready = readiness({ plan: p, kind: current, choice, reason: reasonId || null, estimating });
  const amount = p?.estimate?.amount;
  const money = (v: unknown) => amountText(v, p?.currency);
  const gateway = p?.payment ? (p.payment.label ? `${p.payment.label} (${gatewayName(p.payment.gateway)})` : gatewayName(p.payment.gateway)) : "";

  const submit = async () => {
    if (!ready.ok || !p?.payment) return;
    const offline = p.payment.offline;
    const sends = rights.decides && !offline;
    if (sends) {
      const ok = await confirmDialog({
        title: t("Send {amount} back?", { amount: money(amount) }),
        message: t("{value} sends it to the shopper who paid. This sends money and cannot be undone.", { value: gatewayName(p.payment.gateway) }),
        confirmLabel: t("Send {amount}", { amount: money(amount) }),
        danger: true,
      });
      if (!ok) return;
    }
    setBusy(true);
    setError(null);
    let made: { id: string };
    try {
      made = await api.post<{ id: string }>(docPath("OrderRefund"), { ...asked, reason: reasonId, note: note.trim() || undefined, notify_customer: notify });
    } catch (err) {
      setError(errorMessage(err));
      setBusy(false);
      return;
    }
    if (!sends) {
      toast(offline ? t("Noted. Hand the money back, then say you did.") : t("Asked for. A manager decides."));
      onDone();
      return;
    }
    try {
      await api.post(docPath("OrderRefund", made.id, "actions", "approve_refund"), {});
      toast(t("Approved. The gateway sends it within a few minutes."));
    } catch (err) {
      toast(t("Asked for, but not approved: {err}", { err: errorMessage(err) }), "error");
    }
    onDone();
  };

  const body = plan.error && !p ? (
    <div className="state" role="alert">
      <p className="state-title">{t("The refund cannot be worked out")}</p>
      <p className="muted">{isolate(errorMessage(plan.error))}</p>
      <Button icon="refresh" onClick={() => void plan.refetch()}>
        {t("Try again")}
      </Button>
    </div>
  ) : !p ? (
    <div className="state">
      <Spinner label={t("Looking at the order")} />
      <span className="muted">{t("Looking at the order…")}</span>
    </div>
  ) : !p.payable ? (
    <Alert tone="info">{isolate(p.why ?? "")}</Alert>
  ) : (
    <div className="cm-refund">
      <div className="cm-refund-main stack">
        {error && <Alert tone="error">{isolate(error)}</Alert>}
        <div className="segmented" role="group" aria-label={t("What happens")}>
          <button type="button" aria-pressed={current === "Return"} onClick={() => setKind("Return")}>
            {t("Goods come back")}
          </button>
          <button type="button" aria-pressed={current === "Cancellation"} disabled={!p.cancel?.allowed} title={p.cancel?.allowed ? undefined : t("This order has shipped or is already being refunded")} onClick={() => setKind("Cancellation")}>
            {t("Cancel the order")}
          </button>
        </div>
        {current === "Return" ? (
          <>
            {!p.lines?.some((l) => Number(l.returnable) > 0) ? (
              <Alert tone="info">
                {p.lines?.some((l) => Number(l.delivered) > 0 && !l.invoiced) ? t("Nothing can come back until the order is invoiced: a return credits the invoice.") : t("Nothing on this order can come back: it was not delivered, or it has all come back already.")}
              </Alert>
            ) : (
              <ul className="cm-refund-lines">
                {p.lines
                  ?.filter((l) => Number(l.delivered) > 0)
                  .map((l) => {
                    const max = Number(l.returnable);
                    const qty = choice.qty[l.id] ?? 0;
                    return (
                      <li key={l.id} className="cm-refund-line" data-chosen={qty > 0 || undefined}>
                        <div className="cm-refund-what">
                          <strong>{isolate(l.name ?? "")}</strong>
                          <span className="faint">
                            {isolate(units(Number(l.delivered)))} {(max > 0 ? t("delivered · {max} can come back", { max: units(max) }) : (l.invoiced ? t("delivered · all back already") : t("delivered · not invoiced yet")))}
                          </span>
                        </div>
                        {max > 0 ? (
                          <>
                            <Stepper line={l} value={qty} onChange={(n) => setChoice({ ...choice, qty: { ...choice.qty, [l.id]: n } })} />
                            <label className="check-field cm-shelf">
                              <input type="checkbox" checked={restocks(choice, l.id, reasonRestocks)} disabled={qty <= 0} onChange={(e) => setChoice({ ...choice, restock: { ...choice.restock, [l.id]: e.target.checked } })} />
                              <span>{t("Back on the shelf")}</span>
                            </label>
                          </>
                        ) : null}
                      </li>
                    );
                  })}
              </ul>
            )}
            <label className="check-field">
              <input type="checkbox" checked={choice.shipping} disabled={!p.shipping?.available} onChange={(e) => setChoice({ ...choice, shipping: e.target.checked })} />
              <span>{p.shipping?.available ? t("Refund the delivery charge too") : t("The delivery charge cannot be refunded (there is none, or it went back already)")}</span>
            </label>
          </>
        ) : (
          <Alert tone="warning">
            {t("The order has not shipped. Its whole payment goes back and the order is cancelled, which frees its stock.")}
          </Alert>
        )}
        <Field label={t("Why")} htmlFor="refund-reason" required>
          <select id="refund-reason" className="select" value={reasonId} required onChange={(e) => setReasonId(e.target.value)}>
            <option value="">{t("Choose a reason…")}</option>
            {reasons.data?.map((r) => (
              <option key={r.id} value={r.id}>
                {isolate(r.reason)}
              </option>
            ))}
          </select>
        </Field>
        <Field label={t("Note")} htmlFor="refund-note" hint={t("For whoever decides, and for your own records.")}>
          <textarea id="refund-note" className="textarea" rows={2} maxLength={2000} value={note} onChange={(e) => setNote(e.target.value)} />
        </Field>
      </div>
      <aside className="cm-refund-side" aria-label={t("What goes back")}>
        <div className="cm-refund-sum" aria-live="polite">
          <span className="muted">{t("Goes back to the shopper")}</span>
          <strong className="cm-refund-amount">{amount && Number(amount) > 0 ? money(amount) : "—"}</strong>
          {estimating && <span className="faint">{t("Working it out…")}</span>}
        </div>
        {p.payment && (
          <dl className="kv">
            <div>
              <dt>{t("Through")}</dt>
              <dd>{isolate(gateway)}</dd>
            </div>
            <div>
              <dt>{t("Paid")}</dt>
              <dd>{isolate(money(p.payment.paid))}</dd>
            </div>
            <div>
              <dt>{t("Sent back or asked for")}</dt>
              <dd>{isolate(money(p.payment.held))}</dd>
            </div>
            <div>
              <dt>{t("Can still go back")}</dt>
              <dd>{isolate(money(p.payment.refundable))}</dd>
            </div>
          </dl>
        )}
        {p.payment?.offline && <p className="faint cm-tight">{p.payment.label ?? p.payment.gateway} {t("has no gateway to send money: you hand it back yourself, and this records that you did.")}</p>}
        <label className="check-field">
          <input type="checkbox" checked={notify} onChange={(e) => setNotify(e.target.checked)} />
          <span>{t("Tell the shopper")}</span>
        </label>
        {!ready.ok && ready.why && <p className="cm-refund-why" role="status">{isolate(ready.why)}</p>}
      </aside>
    </div>
  );

  return (
    <Dialog
      open
      wide
      onClose={busy ? () => {} : onClose}
      title={t("Send money back · {value}", { value: order.number ?? "order" })}
      footer={
        <>
          <Button onClick={onClose} disabled={busy}>
            {t("Close")}
          </Button>
          <Button variant="primary" loading={busy} disabled={!ready.ok} onClick={() => void submit()}>
            {p?.payment?.offline ? t("Start the refund") : rights.decides ? (amount && Number(amount) > 0 ? t("Send {amount}", { amount: money(amount) }) : t("Send it back")) : t("Ask for approval")}
          </Button>
        </>
      }
    >
      {body}
    </Dialog>
  );
}
