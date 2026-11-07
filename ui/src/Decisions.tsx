// Refunds and money a gateway took and could not book, as cards that say where each stands and
// offer what a person may do next: approve, decline, say it was refunded another way, post to the
// books. The server holds what may follow each state and who may do it, and says no with its
// reason; a gateway can never approve one of these.

import { type FormEvent, type ReactNode, useState } from "react";
import { Link } from "react-router";
import { api, docPath, errorMessage, relativeTime, t, isolate } from "@ferrum/client";
import { Alert, Badge, Button, confirmDialog, Dialog, Field, Icon, Menu, MenuItem, toast } from "@ferrum/kit";
import { amountText } from "./queues";
import { copy, type Rights } from "./data";
import { type Exception, exceptionWords, gatewayName, type Refund, refundWords, type Words } from "./standing";

const isCash = (gateway?: string | null) => gateway === "Manual" || gateway === "Cash on Delivery";

function Card({ words, title, meta, children, actions, id }: { words: Words; title: ReactNode; meta: ReactNode; children?: ReactNode; actions: ReactNode; id: string }) {
  return (
    <li className="cm-money" data-tone={words.tone} aria-labelledby={id}>
      <div className="cm-money-body">
        <div className="cm-money-title" id={id}>
          {title}
        </div>
        <div className="cm-money-words">
          <Badge tone={words.tone}>{isolate(words.headline)}</Badge>
          {words.detail && <span className="cm-money-detail">{isolate(words.detail)}</span>}
        </div>
        {children}
        <div className="cm-money-meta faint">{meta}</div>
      </div>
      <div className="cm-money-actions">{actions}</div>
    </li>
  );
}

function Reference({ value, what }: { value?: string | null; what: string }) {
  if (!value) return null;
  return (
    <span className="cm-ref">
      <code className="mono">{isolate(value)}</code>
      <button type="button" className="icon-btn icon-btn-sm" aria-label={t("Copy {what}", { what })} title={t("Copy {what}", { what })} onClick={() => void copy(value, what)}>
        <Icon name="copy" size={13} />
      </button>
    </span>
  );
}

/** A dialog asking for words (why it was declined, how it was settled) and, optionally, a reference. */
function Say({ title, label, hint, required, reference, confirm, danger, onClose, onSubmit }: {
  title: string;
  label: string;
  hint?: string;
  required?: boolean;
  reference?: string;
  confirm: string;
  danger?: boolean;
  onClose: () => void;
  onSubmit: (note: string, reference: string) => Promise<void>;
}) {
  const [note, setNote] = useState("");
  const [ref, setRef] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const submit = async (e: FormEvent) => {
    e.preventDefault();
    setBusy(true);
    setError(null);
    try {
      await onSubmit(note.trim(), ref.trim());
    } catch (err) {
      setError(errorMessage(err));
      setBusy(false);
    }
  };
  return (
    <Dialog open onClose={busy ? () => {} : onClose} title={title}>
      <form className="stack" onSubmit={submit}>
        {error && <Alert tone="error">{isolate(error)}</Alert>}
        <Field label={label} htmlFor="say-note" required={required} hint={hint}>
          <textarea id="say-note" className="textarea" rows={3} value={note} maxLength={2000} required={required} onChange={(e) => setNote(e.target.value)} />
        </Field>
        {reference && (
          <Field label={reference} htmlFor="say-ref">
            <input id="say-ref" className="input" value={ref} maxLength={200} autoComplete="off" onChange={(e) => setRef(e.target.value)} />
          </Field>
        )}
        <div className="row cm-end-row">
          <Button onClick={onClose} disabled={busy}>
            {t("Cancel")}
          </Button>
          <Button type="submit" variant={danger ? "danger" : "primary"} loading={busy} disabled={required && !note.trim()}>
            {isolate(confirm)}
          </Button>
        </div>
      </form>
    </Dialog>
  );
}

type Asking = null | "decline" | "settle";

/** Runs one of a record's actions, and refreshes the screen; a refusal reaches the caller with the server's reason. */
async function act<T = unknown>(entity: string, id: string, name: string, params: Record<string, unknown>, then: () => void): Promise<T> {
  const done = await api.post<T>(docPath(entity, id, "actions", name), params);
  then();
  return done;
}

export function RefundRow({ r, rights, showOrder = true, onChange }: { r: Refund; rights: Rights; showOrder?: boolean; onChange: () => void }) {
  const [asking, setAsking] = useState<Asking>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const words = refundWords(r, amountText);
  const cash = isCash(r.gateway);
  const run = (name: string, params: Record<string, unknown> = {}) => act<{ booked?: boolean; booking_error?: string | null; error?: string | null }>("OrderRefund", r.id, name, params, onChange);
  const attempt = async (name: string, good: string) => {
    setBusy(name);
    try {
      const done = await run(name);
      if (done.booked === false || done.error) toast(done.error ?? done.booking_error ?? t("It could not be posted yet."), "error");
      else toast(good);
    } catch (e) {
      toast(errorMessage(e), "error");
    } finally {
      setBusy(null);
    }
  };
  const approve = async () => {
    const ok = await confirmDialog({
      title: t("Send {value} back?", { value: amountText(r.amount, r.currency_code) }),
      message: (
        <>
          <p>
            {isolate(gatewayName(r.gateway))} {t("sends it to the shopper who paid")}{r.order_number ? t(" for order {order_number}", { order_number: r.order_number }) : ""}{t(". This sends money and cannot be undone.")}
          </p>
          {r.note && <p className="muted">“{isolate(r.note)}”</p>}
        </>
      ),
      confirmLabel: t("Send {value}", { value: amountText(r.amount, r.currency_code) }),
      danger: true,
    });
    if (ok) await attempt("approve_refund", t("Approved. The gateway sends it within a few minutes."));
  };
  const can = rights.decides;
  const status = r.status;
  const primary = !can ? null : status === "Requested" || status === "Refund Failed" ? (
    cash && status === "Requested" ? (
      <Button variant="primary" size="sm" icon="check" onClick={() => setAsking("settle")}>
        {t("I refunded it")}
      </Button>
    ) : (
      <Button variant="primary" size="sm" icon="check" loading={busy === "approve_refund"} onClick={() => void approve()}>
        {status === "Refund Failed" ? t("Approve again") : t("Approve")}
      </Button>
    )
  ) : status === "Approved" && cash ? (
    <Button variant="primary" size="sm" icon="check" onClick={() => setAsking("settle")}>
      {t("I refunded it")}
    </Button>
  ) : status === "Refunded" && !r.booked ? (
    <Button variant="primary" size="sm" icon="refresh" loading={busy === "post_books"} onClick={() => void attempt("post_books", t("Posted to the books"))}>
      {t("Try posting again")}
    </Button>
  ) : null;
  const more = can && (status === "Requested" || status === "Refund Failed" || (status === "Approved" && !cash));
  const secondary = more ? (
    <Menu ariaLabel="More ways to settle this refund" icon="chevronDown" buttonClass="btn btn-default btn-sm" label={t("More")}>
      {(close) => (
        <>
          {!(cash && status === "Requested") && (
            <MenuItem
              onSelect={() => {
                close();
                setAsking("settle");
              }}
            >
              {t("I refunded it another way…")}
            </MenuItem>
          )}
          {status !== "Approved" && (
            <MenuItem
              danger
              onSelect={() => {
                close();
                setAsking("decline");
              }}
            >
              {t("Decline…")}
            </MenuItem>
          )}
        </>
      )}
    </Menu>
  ) : null;
  return (
    <>
      <Card
        id={`refund-${r.id}`}
        words={words}
        title={
          <>
            <strong>{isolate(amountText(r.amount, r.currency_code))}</strong>
            <span className="muted"> · {r.kind === "Cancellation" ? t("order cancelled") : t("goods back")}</span>
            {showOrder && r.sales_order && (
              <>
                {isolate(" ")}
                · <Link to={`/o/${r.sales_order}`}>{r.order_number ?? t("order")}</Link>
              </>
            )}
          </>
        }
        meta={
          <>
            {isolate(r.number ?? "")} · {isolate(gatewayName(r.gateway))}
            {r.created_at ? t(" · asked {value}", { value: relativeTime(r.created_at) }) : ""}
            {r.approved_by ? t(" · approved by {approved_by}", { approved_by: r.approved_by }) : ""} <Reference value={r.payment_reference} what="payment reference" />
          </>
        }
        actions={
          <>
            {primary}
            {secondary}
          </>
        }
      >
        {r.note && <p className="cm-money-note">“{isolate(r.note)}”</p>}
        {rights.ready && !can && (r.status === "Requested" || r.status === "Refund Failed") && <p className="faint cm-money-note">{t("A manager decides this.")}</p>}
      </Card>
      {asking === "decline" && (
        <Say
          title={t("Decline this refund?")}
          label={t("Why")}
          hint={t("The person who asked will see this.")}
          confirm="Decline"
          danger
          onClose={() => setAsking(null)}
          onSubmit={async (note) => {
            await run("decline", note ? { note } : {});
            setAsking(null);
          }}
        />
      )}
      {asking === "settle" && (
        <Say
          title={t("You refunded it another way?")}
          label={t("How")}
          hint={t("For instance: cash handed back at the shop, or refunded from the provider's own dashboard. The money is booked as sent back.")}
          required
          reference="Reference (optional)"
          confirm="It is refunded"
          onClose={() => setAsking(null)}
          onSubmit={async (note, ref) => {
            await run("mark_refunded", { note, ...(ref ? { refund_reference: ref } : {}) });
            setAsking(null);
          }}
        />
      )}
    </>
  );
}

export function ExceptionRow({ x, rights, order, onChange }: { x: Exception; rights: Rights; order?: { id: string; number?: string | null } | null; onChange: () => void }) {
  const [asking, setAsking] = useState(false);
  const [busy, setBusy] = useState(false);
  const words = exceptionWords(x, amountText);
  const run = (name: string, params: Record<string, unknown> = {}) => act("PaymentException", x.id, name, params, onChange);
  const approve = async () => {
    const sum = x.amount != null && x.amount !== "" ? amountText(x.amount, x.currency_code) : "this payment";
    const ok = await confirmDialog({
      title: t("Send {sum} back?", { sum }),
      message: t("{value} refunds the whole payment ({value2}) to the payer. This sends money and cannot be undone.", { value: gatewayName(x.gateway), value2: x.gateway_reference ?? "no reference" }),
      confirmLabel: t("Refund it"),
      danger: true,
    });
    if (!ok) return;
    setBusy(true);
    try {
      await run("approve_refund");
      toast(t("Approved. The gateway sends it within a few minutes."));
    } catch (e) {
      toast(errorMessage(e), "error");
    } finally {
      setBusy(false);
    }
  };
  const open = x.status === "Open" || x.status === "Refund Failed";
  return (
    <>
      <Card
        id={`exception-${x.id}`}
        words={words}
        title={
          <>
            <strong>{x.amount != null && x.amount !== "" ? amountText(x.amount, x.currency_code) : t("An amount the gateway did not say")}</strong>
            <span className="muted"> {t("· payment not booked")}</span>
            {(order || x._titles?.payment_request) && (
              <>
                {isolate(" ")}
                · {order ? <Link to={`/o/${order.id}`}>{order.number ?? x._titles?.payment_request}</Link> : x._titles?.payment_request}
              </>
            )}
          </>
        }
        meta={
          <>
            {isolate(gatewayName(x.gateway))}
            {x.created_at ? t(" · reported {value}", { value: relativeTime(x.created_at) }) : ""}
            {x.approved_by ? t(" · approved by {approved_by}", { approved_by: x.approved_by }) : ""} <Reference value={x.gateway_reference} what="payment reference" />
          </>
        }
        actions={
          rights.settles && open ? (
            <>
              <Button variant="primary" size="sm" icon="check" loading={busy} onClick={() => void approve()}>
                {x.status === "Refund Failed" ? t("Approve again") : t("Approve refund")}
              </Button>
              <Button size="sm" onClick={() => setAsking(true)}>
                {t("Settled another way")}
              </Button>
            </>
          ) : rights.settles && x.status === "Refund Approved" ? (
            <Button size="sm" onClick={() => setAsking(true)}>
              {t("Settled another way")}
            </Button>
          ) : null
        }
      >
        {rights.ready && !rights.settles && open && <p className="faint cm-money-note">{t("A manager decides this.")}</p>}
      </Card>
      {asking && (
        <Say
          title={t("Settled another way?")}
          label={t("How it was settled")}
          hint={t("For instance: refunded from the provider's own dashboard on 3 May.")}
          required
          confirm="It is settled"
          onClose={() => setAsking(false)}
          onSubmit={async (note) => {
            await run("resolve", { note });
            setAsking(false);
          }}
        />
      )}
    </>
  );
}
