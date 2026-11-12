// The returns desk: what shoppers asked to send back, sorted into what waits for the desk's decision, what
// was approved and waits for the goods, and what is done. Each request says what was asked and why, what
// the store's terms were when it was asked (where the refund goes, who pays the return shipping, the
// restocking fee) and whether it came inside the window; approving, declining and receiving the goods are
// the server's actions, and receiving shows what the refund will come to before it is confirmed.

import { useQuery, useQueryClient } from "@tanstack/react-query";
import { type FormEvent, useCallback, useState } from "react";
import { Link, useSearchParams } from "react-router";
import { api, docPath, refusal, formatDate, isolate, relativeTime, t } from "@ferrum/client";
import { Alert, Badge, Button, Dialog, EmptyState, ErrorState, Field, FormSkeleton, ListSkeleton, StatePanel, statusTone, toast, useLiveEntity, useShellTitle } from "@ferrum/kit";
import { f, list, soft, useRights } from "./data";
import { amountText, units } from "./queues";
import { destinationWords, feeWords, planLines, refundSum, RETURN_TONES, type ReturnRequest, shippingWords, stage, STAGES, type Stage, windowWords } from "./returnrules";
import { Tabs } from "./Tabs";

const FIELDS = "number,order_number,sales_order,email,status,reason,shopper_note,resolution,shipping_paid_by,restocking_fee_percent,decision_note,decided_by,refund,requested_at,created_at";
const LIMIT = 100;

const NOTHING: Record<Stage, [string, string]> = {
  get decide(): [string, string] { return [t("Nothing to decide"), t("Return requests from shoppers show up here.")]; },
  get receive(): [string, string] { return [t("No goods are on their way back"), t("Requests you approve wait here until the goods arrive.")]; },
  get done(): [string, string] { return [t("Nothing finished yet"), t("Requests that were declined, received or refunded show here.")]; },
};

/** What the request is about, read whole: its lines, and when the goods were delivered, for the window. */
function useDetail(r: ReturnRequest, enabled: boolean) {
  return useQuery({
    enabled,
    queryKey: ["commerce", "return", r.id, "detail"],
    queryFn: async ({ signal }) => {
      const [doc, delivered] = await Promise.all([
        api.get<ReturnRequest>(docPath("ReturnRequest", r.id), undefined, signal),
        r.sales_order ? soft(list<{ posting_date?: string | null }>("DeliveryNote", { filters: f([["items.sales_order", "=", r.sales_order], ["docstatus", "=", 1], ["is_return", "=", false]]), fields: "posting_date", sort: "posting_date desc", limit: 1 }, signal)) : Promise.resolve([]),
      ]);
      return { lines: doc.lines ?? [], delivered: delivered[0]?.posting_date ?? null };
    },
  });
}

function Lines({ r, window: windowDays }: { r: ReturnRequest; window: number | null }) {
  const q = useDetail(r, true);
  if (q.error) return <ErrorState error={q.error} onRetry={() => void q.refetch()} />;
  if (!q.data) return <ListSkeleton rows={2} />;
  const w = windowWords(q.data.delivered, r.requested_at ?? r.created_at, windowDays);
  return (
    <div className="stack">
      <ul className="cm-docs">
        {q.data.lines.map((l, i) => (
          <li key={l.id ?? i}>
            <span>
              {isolate(l.item_name ?? l.sales_order_line)} <strong>× {isolate(units(Number(l.qty)))}</strong>
            </span>
          </li>
        ))}
      </ul>
      {w.words && <p className={w.inside === false ? "cm-late" : "faint"}>{isolate(w.words)}</p>}
    </div>
  );
}

/** Approving (what the shopper is told) or declining (why), with the terms the shopper will read. */
function Decide({ r, mode, onClose, onDone }: { r: ReturnRequest; mode: "approve" | "decline"; onClose: () => void; onDone: () => void }) {
  const [note, setNote] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const approve = mode === "approve";
  const submit = async (e: FormEvent) => {
    e.preventDefault();
    if (busy) return;
    setBusy(true);
    setError(null);
    try {
      await api.post(docPath("ReturnRequest", r.id, "actions", approve ? "approve_return" : "decline_return"), { note: note.trim() || undefined });
      toast(approve ? t("Approved. The shopper is told how to send it back.") : t("Declined. The shopper is told why."));
      onDone();
    } catch (err) {
      setError(refusal(err));
      setBusy(false);
    }
  };
  return (
    <Dialog open onClose={busy ? () => {} : onClose} title={approve ? t("Approve return {value}", { value: r.number ?? "" }) : t("Decline return {value}", { value: r.number ?? "" })}>
      <form className="stack" onSubmit={submit}>
        {error && <Alert tone="error">{isolate(error)}</Alert>}
        {approve && (
          <ul className="cm-tight">
            <li>{isolate(destinationWords(r.resolution))}</li>
            <li>{isolate(shippingWords(r.shipping_paid_by))}</li>
            <li>{isolate(feeWords(r.restocking_fee_percent))}</li>
          </ul>
        )}
        <Field label={approve ? t("What the shopper is told") : t("Why")} htmlFor="decide-note" required={!approve} hint={approve ? t("Where to send it, and anything else they should know.") : t("The shopper reads this.")}>
          <textarea id="decide-note" className="textarea" rows={3} maxLength={2000} value={note} required={!approve} onChange={(e) => setNote(e.target.value)} />
        </Field>
        <div className="row cm-end-row">
          <Button onClick={onClose} disabled={busy}>
            {t("Cancel")}
          </Button>
          <Button type="submit" variant={approve ? "primary" : "danger"} loading={busy} disabled={!approve && !note.trim()}>
            {approve ? t("Approve") : t("Decline")}
          </Button>
        </div>
      </form>
    </Dialog>
  );
}

interface Plan {
  currency?: string;
  estimate?: { amount: string; errors: string[] } | null;
  payable?: boolean;
  why?: string;
}

/** The goods are here: what the refund comes to, where it goes and who pays the shipping, before it is made. */
function Receive({ r, onClose, onDone }: { r: ReturnRequest; onClose: () => void; onDone: () => void }) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const detail = useDetail(r, true);
  const lines = detail.data?.lines ?? [];
  const plan = useQuery({
    enabled: !!detail.data && !!r.sales_order,
    queryKey: ["commerce", "return", r.id, "plan"],
    queryFn: ({ signal }) => api.post<Plan>("/api/app/commerce/desk/refund-plan", { sales_order: r.sales_order, kind: "Return", include_shipping: false, lines: planLines(lines) }, { signal }),
    retry: false,
  });
  const code = plan.data?.currency;
  const money = (v: unknown) => amountText(v, code);
  const credit = Number(plan.data?.estimate?.amount ?? NaN);
  const sum = Number.isFinite(credit) ? refundSum(credit, r.restocking_fee_percent) : null;
  const refused = plan.data?.estimate?.errors?.[0] ?? (plan.data && plan.data.payable === false ? plan.data.why : null);
  const submit = async () => {
    if (busy) return;
    setBusy(true);
    setError(null);
    try {
      await api.post(docPath("ReturnRequest", r.id, "actions", "receive_return"), {});
      toast(t("Goods received. The refund is asked for: a person approves it."));
      onDone();
    } catch (err) {
      setError(refusal(err));
      setBusy(false);
    }
  };
  return (
    <Dialog
      open
      onClose={busy ? () => {} : onClose}
      title={t("The goods for {value} are here", { value: r.number ?? "" })}
      footer={
        <>
          <Button onClick={onClose} disabled={busy}>
            {t("Not yet")}
          </Button>
          <Button variant="primary" loading={busy} disabled={!detail.data} onClick={() => void submit()}>
            {t("Receive the goods and ask for the refund")}
          </Button>
        </>
      }
    >
      <div className="stack">
        {error && <Alert tone="error">{isolate(error)}</Alert>}
        {detail.error ? (
          <ErrorState error={detail.error} onRetry={() => void detail.refetch()} />
        ) : !detail.data ? (
          <FormSkeleton fields={2} />
        ) : (
          <>
            <ul className="cm-docs">
              {lines.map((l, i) => (
                <li key={l.id ?? i}>
                  {isolate(l.item_name ?? l.sales_order_line)} <strong>× {isolate(units(Number(l.qty)))}</strong>
                </li>
              ))}
            </ul>
            <dl className="kv" aria-label={t("What the refund comes to")}>
              <div>
                <dt>{t("Goes")}</dt>
                <dd>{isolate(destinationWords(r.resolution))}</dd>
              </div>
              <div>
                <dt>{t("Return shipping")}</dt>
                <dd>{isolate(shippingWords(r.shipping_paid_by))}</dd>
              </div>
              <div>
                <dt>{t("Restocking fee")}</dt>
                <dd>{isolate(feeWords(r.restocking_fee_percent))}</dd>
              </div>
              {sum && (
                <>
                  <div>
                    <dt>{t("The goods credit about")}</dt>
                    <dd>{isolate(money(sum.credit))}</dd>
                  </div>
                  {sum.fee > 0 && (
                    <div>
                      <dt>{t("Less the fee")}</dt>
                      <dd>{isolate(`−${money(sum.fee)}`)}</dd>
                    </div>
                  )}
                  <div>
                    <dt>
                      <strong>{t("Goes back about")}</strong>
                    </dt>
                    <dd>
                      <strong>{isolate(money(sum.net))}</strong>
                    </dd>
                  </div>
                </>
              )}
            </dl>
            {plan.isPending && !!r.sales_order && <p className="faint">{t("Working out the amount…")}</p>}
            {refused && <Alert tone="warning">{isolate(refused)}</Alert>}
            {plan.error && <p className="faint">{t("The amount could not be worked out here; the server works out the exact one when you confirm.")}</p>}
            <p className="faint cm-tight">{t("Figures are an estimate: the credit note the server makes is the exact one. The refund still waits for a person to approve it.")}</p>
          </>
        )}
      </div>
    </Dialog>
  );
}

function Request({ r, window: windowDays, may, onChange }: { r: ReturnRequest; window: number | null; may: boolean; onChange: () => void }) {
  const [open, setOpen] = useState(false);
  const [asking, setAsking] = useState<null | "approve" | "decline" | "receive">(null);
  const s = stage(r);
  const done = () => {
    setAsking(null);
    onChange();
  };
  return (
    <li className="cm-money" aria-labelledby={`rr-${r.id}`}>
      <div className="cm-money-body">
        <div className="cm-money-title" id={`rr-${r.id}`}>
          {isolate(r.number ?? "")}
          {r.sales_order && (
            <>
              {isolate(" · ")}
              <Link to={`/o/${r.sales_order}`}>{isolate(r.order_number ?? "")}</Link>
            </>
          )}
          {isolate(` · ${r.email ?? ""}`)}
        </div>
        <div className="cm-money-words">
          <Badge tone={statusTone(r.status, RETURN_TONES)}>{isolate(r.status ?? "")}</Badge>
          {r._titles?.reason && <span className="cm-money-detail">{isolate(r._titles.reason)}</span>}
        </div>
        {r.shopper_note && <p className="cm-note">{isolate(r.shopper_note)}</p>}
        {r.decision_note && <p className="faint">{isolate(r.decision_note)}</p>}
        <ul className="faint cm-tight">
          <li>{isolate(destinationWords(r.resolution))}</li>
          <li>{isolate(shippingWords(r.shipping_paid_by))}</li>
          <li>{isolate(feeWords(r.restocking_fee_percent))}</li>
        </ul>
        {open && <Lines r={r} window={windowDays} />}
        <div className="cm-money-meta faint">
          <button type="button" className="link-btn" aria-expanded={open} onClick={() => setOpen(!open)}>
            {open ? t("Hide the goods") : t("Show the goods and the window")}
          </button>
          {isolate(" · ")}
          <time dateTime={r.requested_at ?? r.created_at ?? undefined} title={r.requested_at ? formatDate(r.requested_at) : undefined}>
            {r.requested_at || r.created_at ? t("Asked {value}", { value: relativeTime((r.requested_at ?? r.created_at) as string) }) : ""}
          </time>
          {r.decided_by ? isolate(` · ${r.decided_by}`) : null}
        </div>
      </div>
      <div className="cm-money-actions">
        {s === "decide" && may && (
          <>
            <Button variant="primary" size="sm" icon="check" onClick={() => setAsking("approve")}>
              {t("Approve…")}
            </Button>
            <Button size="sm" onClick={() => setAsking("decline")}>
              {t("Decline…")}
            </Button>
          </>
        )}
        {s === "receive" && may && (
          <Button variant="primary" size="sm" icon="check" onClick={() => setAsking("receive")}>
            {t("Goods arrived…")}
          </Button>
        )}
        {(s === "decide" || s === "receive") && !may && <span className="faint">{t("A manager decides this.")}</span>}
        {r.refund && (
          <Link to={`/money`} className="faint">
            {t("The refund is on the Money tab")}
          </Link>
        )}
      </div>
      {(asking === "approve" || asking === "decline") && <Decide r={r} mode={asking} onClose={() => setAsking(null)} onDone={done} />}
      {asking === "receive" && <Receive r={r} onClose={() => setAsking(null)} onDone={done} />}
    </li>
  );
}

export function Returns() {
  useShellTitle(t("Returns"));
  const rights = useRights();
  const qc = useQueryClient();
  const [sp, setSp] = useSearchParams();
  const tab = (STAGES.find((s) => s.id === sp.get("tab"))?.id ?? "decide") as Stage;
  const q = useQuery({
    enabled: rights.seesReturns,
    queryKey: ["commerce", "returns"],
    queryFn: async ({ signal }) => {
      const [rows, window] = await Promise.all([
        list<ReturnRequest>("ReturnRequest", { fields: FIELDS, sort: "created_at desc", limit: LIMIT, titles: "true" }, signal).then((r) => r.rows),
        soft(list<{ return_window_days?: number | string | null }>("StoreSettings", { fields: "return_window_days", limit: 1 }, signal)).then((r) => (r[0]?.return_window_days == null ? null : Number(r[0].return_window_days))),
      ]);
      return { rows, window };
    },
  });
  const refresh = useCallback(() => void qc.invalidateQueries({ queryKey: ["commerce"] }), [qc]);
  useLiveEntity("ReturnRequest", refresh);
  useLiveEntity("OrderRefund", refresh);

  if (!rights.ready) return <ListSkeleton rows={4} />;
  if (!rights.seesReturns) return <StatePanel kind="denied">{t("Return requests are for the store's managers. Ask an administrator for the Website Manager role.")}</StatePanel>;
  const rows = (q.data?.rows ?? []).filter((r) => stage(r) === tab);
  const size = (s: Stage) => (q.data?.rows ?? []).filter((r) => stage(r) === s).length;
  const body = q.error ? (
    <ErrorState error={q.error} onRetry={() => void q.refetch()} />
  ) : !q.data ? (
    <ListSkeleton rows={4} />
  ) : rows.length === 0 ? (
    <EmptyState title={NOTHING[tab][0]} icon="check">
      {isolate(NOTHING[tab][1])}
    </EmptyState>
  ) : (
    <ul className="cm-moneys">
      {rows.map((r) => (
        <Request key={r.id} r={r} window={q.data.window} may={rights.decidesReturns} onChange={refresh} />
      ))}
    </ul>
  );
  return (
    <div className="cm-page">
      <header className="cm-head">
        <div>
          <Tabs />
          <p className="page-sub" aria-live="polite">
            {q.data ? (size("decide") ? t("{value} waiting for a decision", { value: size("decide") }) : t("Nothing is waiting for a decision")) : " "}
          </p>
        </div>
      </header>
      <div className="cm-stages" role="tablist" aria-label={t("Returns")}>
        {STAGES.map((s) => (
          <button key={s.id} type="button" role="tab" aria-selected={tab === s.id} onClick={() => setSp(s.id === "decide" ? {} : { tab: s.id }, { replace: true })}>
            {isolate(s.label)}
            {q.data && s.id !== "done" && <span className="cm-stage-n">{size(s.id)}</span>}
          </button>
        ))}
      </div>
      {body}
      {q.data && q.data.rows.length >= LIMIT && <p className="faint">{t("Showing the latest {LIMIT}.", { LIMIT })}</p>}
    </div>
  );
}
