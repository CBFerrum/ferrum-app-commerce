// Money that needs a person, on one screen: refunds waiting for a decision, refunds the gateway is
// sending, and payments a gateway took and could not book, so that a manager can approve a refund
// or say how it was settled. A gateway never approves one: it only carries out what a person
// approved. What is waiting shows first, and on the tab.

import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useCallback, useMemo } from "react";
import { useSearchParams } from "react-router";
import { EmptyState, ErrorState, PageLoading, Spinner, useLiveEntity, useShellTitle } from "@ferrum/kit";
import { f, list, soft, useRights } from "./data";
import { ExceptionRow, RefundRow } from "./Decisions";
import { BUCKETS, type Bucket, exceptionBucket, type Exception, type Refund, refundBucket } from "./standing";
import { Tabs } from "./Tabs";
import { t } from "@ferrum/client";

interface Request {
  id: string;
  sales_order?: string | null;
  order_number?: string | null;
}

const REFUND_FIELDS =
  "number,sales_order,order_number,kind,amount,currency_code,gateway,status,requested_by,approved_by,approved_at,refunded_at,refund_reference,refund_error,booked,booking_error,decision_note,note,payment_reference,created_at";
const EXCEPTION_FIELDS = "gateway,gateway_reference,payment_request,amount,currency_code,reason,status,approved_by,approved_at,refund_reference,refund_error,note,created_at";
const LIMIT = 100;

const NOTHING: Record<Bucket, [string, string]> = {
  decide: ["Nothing needs a decision", "No refund is waiting, and every payment a gateway took has been booked."],
  waiting: ["Nothing is with a gateway", "Refunds you approve show here until the gateway has sent them."],
  done: ["Nothing finished yet", "Refunds that were sent, declined or settled show here."],
};

export function Money() {
  useShellTitle("Money");
  const rights = useRights();
  const qc = useQueryClient();
  const [sp, setSp] = useSearchParams();
  const tab = (BUCKETS.find((b) => b.id === sp.get("tab"))?.id ?? "decide") as Bucket;
  const q = useQuery({
    enabled: rights.seesMoney,
    queryKey: ["commerce", "money"],
    queryFn: async ({ signal }) => {
      const [refunds, exceptions] = await Promise.all([
        soft(list<Refund>("OrderRefund", { fields: REFUND_FIELDS, sort: "created_at desc", limit: LIMIT, titles: "true" }, signal)),
        soft(list<Exception>("PaymentException", { fields: EXCEPTION_FIELDS, sort: "created_at desc", limit: LIMIT, titles: "true" }, signal)),
      ]);
      const wanted = [...new Set(exceptions.map((x) => x.payment_request).filter((v): v is string => !!v))];
      const requests = wanted.length ? await soft(list<Request>("PaymentRequest", { filters: f([["id", "in", wanted]]), fields: "sales_order,order_number", limit: LIMIT }, signal)) : [];
      return { refunds, exceptions, orders: new Map(requests.map((r) => [r.id, r])) };
    },
  });
  const refresh = useCallback(() => void qc.invalidateQueries({ queryKey: ["commerce"] }), [qc]);
  useLiveEntity("OrderRefund", refresh);
  useLiveEntity("PaymentException", refresh);
  useLiveEntity("PaymentRequest", refresh);

  const buckets = useMemo(() => {
    const out: Record<Bucket, { refunds: Refund[]; exceptions: Exception[] }> = { decide: { refunds: [], exceptions: [] }, waiting: { refunds: [], exceptions: [] }, done: { refunds: [], exceptions: [] } };
    for (const r of q.data?.refunds ?? []) out[refundBucket(r)].refunds.push(r);
    for (const x of q.data?.exceptions ?? []) out[exceptionBucket(x)].exceptions.push(x);
    return out;
  }, [q.data]);

  if (!rights.ready) return <PageLoading />;
  if (!rights.seesMoney) {
    return (
      <EmptyState title={t("Nothing here for you")} icon="store">
        <p className="muted">{t("Refunds and payments that need a decision are for the store's managers. Ask an administrator for the Website Manager role.")}</p>
      </EmptyState>
    );
  }
  const here = buckets[tab];
  const size = (b: Bucket) => buckets[b].refunds.length + buckets[b].exceptions.length;
  const body = q.error ? (
    <ErrorState error={q.error} onRetry={() => void q.refetch()} />
  ) : !q.data ? (
    <Spinner label={t("Loading refunds and payments")} />
  ) : size(tab) === 0 ? (
    <EmptyState title={NOTHING[tab][0]} icon="check">
      {NOTHING[tab][1]}
    </EmptyState>
  ) : (
    <>
      {here.refunds.length > 0 && (
        <section aria-labelledby="m-refunds">
          <h2 className="cm-group" id="m-refunds">
            {t("Refunds")}
          </h2>
          <ul className="cm-moneys">
            {here.refunds.map((r) => (
              <RefundRow key={r.id} r={r} rights={rights} onChange={refresh} />
            ))}
          </ul>
        </section>
      )}
      {here.exceptions.length > 0 && (
        <section aria-labelledby="m-exceptions">
          <h2 className="cm-group" id="m-exceptions">
            {t("Payments the gateway took and could not book")}
          </h2>
          <ul className="cm-moneys">
            {here.exceptions.map((x) => {
              const request = x.payment_request ? q.data.orders.get(x.payment_request) : undefined;
              return <ExceptionRow key={x.id} x={x} rights={rights} order={request?.sales_order ? { id: request.sales_order, number: request.order_number } : null} onChange={refresh} />;
            })}
          </ul>
        </section>
      )}
      {tab === "done" && q.data.refunds.length + q.data.exceptions.length >= LIMIT && <p className="faint">{t("Showing the latest {LIMIT} of each.", { LIMIT })}</p>}
    </>
  );

  return (
    <div className="cm-page">
      <header className="cm-head">
        <div>
          <Tabs />
          <p className="page-sub" aria-live="polite">
            {q.data ? (size("decide") ? `${size("decide")} waiting for a decision` : "Nothing is waiting for a decision") : " "}
          </p>
        </div>
      </header>
      <div className="cm-stages" role="tablist" aria-label={t("Money")}>
        {BUCKETS.map((b) => (
          <button key={b.id} type="button" role="tab" aria-selected={tab === b.id} onClick={() => setSp(b.id === "decide" ? {} : { tab: b.id }, { replace: true })}>
            {b.label}
            {q.data && b.id !== "done" && <span className="cm-stage-n">{size(b.id)}</span>}
          </button>
        ))}
      </div>
      {body}
    </div>
  );
}
