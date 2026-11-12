// Stock alerts: shoppers who asked to be told when an out of stock item is back, by item, with how many
// wait and how long the longest has waited. The shopper is told once, by the server, when the item is back
// in stock; nothing here sends it. A shopper's email is personal data, so it is shown only when the row is
// opened.

import { useQuery } from "@tanstack/react-query";
import { useState } from "react";
import { Link, useSearchParams } from "react-router";
import { isolate, relativeTime, t } from "@ferrum/client";
import { Badge, EmptyState, ErrorState, ListSkeleton, StatePanel, useShellTitle } from "@ferrum/kit";
import { list, useRights } from "./data";
import { type Alert, groups } from "./returnrules";
import { Tabs } from "./Tabs";

const FIELDS = "website_item,item_title,item_slug,email,status,sent_at,created_at";
const LIMIT = 500;

function Group({ g, tab }: { g: ReturnType<typeof groups>[number]; tab: "waiting" | "sent" }) {
  const [open, setOpen] = useState(false);
  const rows = tab === "waiting" ? g.waiting : g.sent;
  return (
    <li className="cm-money">
      <div className="cm-money-body">
        <div className="cm-money-title">
          {g.item ? <Link to={`/e/WebsiteItem/${g.item}`}>{isolate(g.title)}</Link> : isolate(g.title)}
        </div>
        <div className="cm-money-words">
          <Badge tone={tab === "waiting" ? "amber" : "green"}>{tab === "waiting" ? t("{count} waiting", { count: g.waiting.length }) : t("{count} told", { count: g.sent.length })}</Badge>
          {tab === "waiting" && g.oldest && <span className="cm-money-detail">{t("The longest has waited since {value}", { value: relativeTime(g.oldest) })}</span>}
        </div>
        {open && (
          <ul className="cm-docs">
            {rows.map((a) => (
              <li key={a.id}>
                {isolate(a.email ?? "")} <span className="faint">{a.created_at ? relativeTime(a.created_at) : ""}</span>
              </li>
            ))}
          </ul>
        )}
        <div className="cm-money-meta faint">
          <button type="button" className="link-btn" aria-expanded={open} onClick={() => setOpen(!open)}>
            {open ? t("Hide the shoppers") : t("Show the shoppers")}
          </button>
        </div>
      </div>
    </li>
  );
}

export function Alerts() {
  useShellTitle(t("Stock alerts"));
  const rights = useRights();
  const [sp, setSp] = useSearchParams();
  const tab = sp.get("tab") === "sent" ? "sent" : "waiting";
  const q = useQuery({ enabled: rights.seesReturns, queryKey: ["commerce", "alerts"], queryFn: ({ signal }) => list<Alert>("StockAlert", { fields: FIELDS, sort: "created_at asc", limit: LIMIT }, signal).then((r) => r.rows) });
  if (!rights.ready) return <ListSkeleton rows={4} />;
  if (!rights.seesReturns) return <StatePanel kind="denied">{t("Stock alerts are for the store's managers. Ask an administrator for the Website Manager role.")}</StatePanel>;
  const all = groups(q.data ?? []);
  const shown = all.filter((g) => (tab === "waiting" ? g.waiting.length : g.sent.length) > 0);
  const waiting = all.reduce((n, g) => n + g.waiting.length, 0);
  const body = q.error ? (
    <ErrorState error={q.error} onRetry={() => void q.refetch()} />
  ) : !q.data ? (
    <ListSkeleton rows={4} />
  ) : shown.length === 0 ? (
    <EmptyState title={tab === "waiting" ? t("Nobody is waiting for anything") : t("Nobody has been told yet")} icon="check">
      {tab === "waiting" ? t("When a shopper leaves their email on an out of stock item, it shows here.") : t("Shoppers who were told an item is back show here.")}
    </EmptyState>
  ) : (
    <ul className="cm-moneys">
      {shown.map((g) => (
        <Group key={g.key} g={g} tab={tab} />
      ))}
    </ul>
  );
  return (
    <div className="cm-page">
      <header className="cm-head">
        <div>
          <Tabs />
          <p className="page-sub" aria-live="polite">
            {q.data ? t("{value} waiting for items to come back", { value: waiting }) : " "}
          </p>
        </div>
      </header>
      <div className="cm-stages" role="tablist" aria-label={t("Stock alerts")}>
        {(["waiting", "sent"] as const).map((id) => (
          <button key={id} type="button" role="tab" aria-selected={tab === id} onClick={() => setSp(id === "waiting" ? {} : { tab: id }, { replace: true })}>
            {id === "waiting" ? t("Waiting") : t("Told")}
          </button>
        ))}
      </div>
      {body}
      {q.data && q.data.length >= LIMIT && <p className="faint">{t("Showing the oldest {LIMIT}.", { LIMIT })}</p>}
    </div>
  );
}
