// Changing an order that has not been fulfilled: quantities, lines added and taken away, where it goes and
// by which delivery method. What saving would do is asked of the server before anything is saved (the preview
// route runs the save's own code and throws it away): the lines as priced, the totals, the difference and what
// happens to it. Saving shows the same figures. Nothing about the rules lives here: the server refuses what has
// shipped or been billed, and its refusal is shown as it is.

import { keepPreviousData, useQuery } from "@tanstack/react-query";
import { useState } from "react";
import { ApiError, api, docPath, refusal, isolate, t } from "@ferrum/client";
import { Alert, Button, Dialog, Field, ListSkeleton, toast, useDebounced } from "@ferrum/kit";
import { LinkInput } from "@ferrum/records";
import { type Address, type Draft, emptyAddress, emptyDraft, type OrderLine, previewBody, quantity, readiness, type Saved, savedWords, settlementWords, isCharge, lineProblem, updateParams } from "./edit";
import { f, list, soft } from "./data";
import { amountText, units } from "./queues";
import { gatewayName } from "./standing";

interface Order {
  id: string;
  number?: string | null;
  commerce_card_held?: string | number | null;
}

interface Rule {
  id: string;
  label?: string | null;
}

const PREVIEW = "/api/app/commerce/desk/change-preview";

/** What saving would come to, as the server worked it out: the lines as priced, the totals, the difference and what happens to it. */
function Worked({ p, code, approve, setApprove }: { p: Saved; code?: string | null; approve: boolean; setApprove: (v: boolean) => void }) {
  const money = (v: unknown) => amountText(v, code);
  const diff = Number(p.difference ?? 0);
  const s = p.settlement ?? { kind: "none" as const };
  const words = settlementWords(s, money, (gateway, label) => (label ? `${label} (${gatewayName(gateway)})` : gateway ? gatewayName(gateway) : t("the store's first payment method")));
  const t0 = p.totals;
  return (
    <div className="stack">
      <div className="cm-refund-sum" aria-live="polite">
        <span className="muted">{diff > 0 ? t("The order will cost more by") : diff < 0 ? t("The order will cost less by") : t("The order's total")}</span>
        <strong className="cm-refund-amount">{diff === 0 ? isolate(money(p.total)) : isolate(money(Math.abs(diff)))}</strong>
        {t0 && <span className="faint">{t("Before it was {amount}.", { amount: money(t0.was) })}</span>}
      </div>
      <table className="table cm-table" aria-label={t("The lines as priced")}>
        <thead>
          <tr>
            <th>{t("Item")}</th>
            <th className="num">{t("Qty")}</th>
            <th className="num">{t("Amount")}</th>
          </tr>
        </thead>
        <tbody>
          {(p.lines ?? []).map((l) => (
            <tr key={`${l.line}-${l.id ?? l.item}`}>
              <td data-label={t("Item")}>
                {isolate(l.name ?? "")}
                {l.delivery && <span className="faint"> · {t("Delivery")}</span>}
              </td>
              <td className="num" data-label={t("Qty")}>
                {isolate(units(Number(l.qty)))}
              </td>
              <td className="num" data-label={t("Amount")}>
                {isolate(money(l.amount))}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
      {t0 && (
        <dl className="kv">
          <div>
            <dt>{t("Goods")}</dt>
            <dd>{isolate(money(t0.goods))}</dd>
          </div>
          <div>
            <dt>{t("Delivery")}</dt>
            <dd>{Number(t0.delivery) === 0 ? t("Free") : isolate(money(t0.delivery))}</dd>
          </div>
          <div>
            <dt>{t("Taxes")}</dt>
            <dd>{isolate(money(t0.taxes))}</dd>
          </div>
          <div>
            <dt>{t("Total")}</dt>
            <dd>
              <strong>{isolate(money(t0.rounded_total))}</strong>
            </dd>
          </div>
        </dl>
      )}
      <p className="cm-tight" role="status">
        {isolate(words.text)}
      </p>
      {words.approvable && (
        <label className="check-field">
          <input type="checkbox" checked={approve} onChange={(e) => setApprove(e.target.checked)} />
          <span>{t("Send the refund now (otherwise it waits for a person to approve it)")}</span>
        </label>
      )}
    </div>
  );
}

function AddressFields({ a, onChange }: { a: Address; onChange: (a: Address) => void }) {
  const set = (k: keyof Address) => (e: { target: { value: string } }) => onChange({ ...a, [k]: e.target.value });
  return (
    <div className="stack">
      <Field label={t("Street")} htmlFor="edit-line1" required>
        <input id="edit-line1" className="input" value={a.line1} maxLength={200} autoComplete="off" onChange={set("line1")} />
      </Field>
      <Field label={t("Street, line 2")} htmlFor="edit-line2">
        <input id="edit-line2" className="input" value={a.line2} maxLength={200} autoComplete="off" onChange={set("line2")} />
      </Field>
      <div className="cm-row3">
        <Field label={t("City")} htmlFor="edit-city" required>
          <input id="edit-city" className="input" value={a.city} maxLength={200} autoComplete="off" onChange={set("city")} />
        </Field>
        <Field label={t("State or region")} htmlFor="edit-state">
          <input id="edit-state" className="input" value={a.state} maxLength={200} autoComplete="off" onChange={set("state")} />
        </Field>
        <Field label={t("Postal code")} htmlFor="edit-postal">
          <input id="edit-postal" className="input" value={a.postal_code} maxLength={200} autoComplete="off" onChange={set("postal_code")} />
        </Field>
      </div>
      <Field label={t("Country")} htmlFor="edit-country" required hint={t("Two letters, such as IN or DE. Tax follows where the order now goes.")}>
        <input id="edit-country" className="input" value={a.country} maxLength={2} autoComplete="off" onChange={set("country")} />
      </Field>
    </div>
  );
}

export function EditOrderDialog({ order, lines, shippingItem, code, onClose, onDone }: { order: Order; lines: OrderLine[]; shippingItem?: string | null; code?: string | null; onClose: () => void; onDone: () => void }) {
  const [draft, setDraft] = useState<Draft>(emptyDraft);
  const [rule, setRule] = useState("");
  const [note, setNote] = useState("");
  const [notify, setNotify] = useState(true);
  const [approve, setApprove] = useState(true);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [next, setNext] = useState(1);
  const held = Number(order.commerce_card_held ?? 0) > 0;
  const charge = (l: OrderLine) => isCharge(l, shippingItem);
  const ready = readiness(lines, draft, charge);
  const money = (v: unknown) => amountText(v, code);
  const rules = useQuery({ queryKey: ["commerce", "shipping-rules"], queryFn: ({ signal }) => soft(list<Rule>("ShippingRule", { filters: f([["enabled", "=", true]]), fields: "label", sort: "sort_order asc", limit: 50 }, signal)) });

  // What saving would do, asked of the server a moment after the last edit; the same body is what saving sends.
  const asked = JSON.stringify(previewBody(order.id, lines, draft, rule));
  const key = useDebounced(asked, 300);
  const preview = useQuery({
    // Only once the edit has settled: a debounce still catching up would ask about an older body.
    enabled: ready.ok && !held && key === asked,
    queryKey: ["commerce", "change-preview", key],
    queryFn: ({ signal }) => api.post<Saved>(PREVIEW, JSON.parse(key) as unknown, { signal }),
    placeholderData: keepPreviousData,
    gcTime: 0,
  });
  const p = ready.ok && !held ? preview.data : undefined;
  const working = ready.ok && !held && (preview.isFetching || key !== asked);
  // The server asks for a choice when more than one delivery method reaches the place.
  const needsRule = preview.error instanceof ApiError && preview.error.fields.some((x) => x.field === "shipping_rule");
  const refused = ready.ok && !held && !working && preview.error ? refusal(preview.error) : null;
  const canSave = ready.ok && !held && !!p && !working && !preview.error;

  const setQty = (id: string, v: string) => setDraft((d) => ({ ...d, qty: { ...d.qty, [id]: v } }));

  const submit = async () => {
    if (!canSave || busy) return;
    setBusy(true);
    setError(null);
    try {
      const saved = await api.post<Saved>(docPath("SalesOrder", order.id, "actions", "web_update_items"), { ...updateParams(lines, draft, note, notify, approve), shipping_rule: rule || undefined });
      // The same figures the preview showed; if the prices moved in between, the desk is told so.
      toast(savedWords(saved, money) + (p && saved.total !== p.total ? ` ${t("The price moved since the preview: it was {amount}.", { amount: money(p.total) })}` : ""));
      onDone();
    } catch (err) {
      setError(refusal(err));
      setBusy(false);
    }
  };

  return (
    <Dialog
      open
      wide
      onClose={busy ? () => {} : onClose}
      title={t("Change {value}", { value: order.number ?? t("order") })}
      footer={
        <>
          <Button onClick={onClose} disabled={busy}>
            {t("Close")}
          </Button>
          <Button variant="primary" loading={busy} disabled={!canSave} onClick={() => void submit()}>
            {t("Save the change")}
          </Button>
        </>
      }
    >
      <div className="cm-refund">
        <div className="cm-refund-main stack">
          {error && <Alert tone="error">{isolate(error)}</Alert>}
          {held && <Alert tone="warning">{t("A gift card holds part of this order until the rest is paid: it cannot be changed until then.")}</Alert>}
          <table className="table cm-table">
            <thead>
              <tr>
                <th>{t("Item")}</th>
                <th className="num">{t("Price")}</th>
                <th className="num">{t("Qty")}</th>
                <th>
                  <span className="sr-only">{t("Remove")}</span>
                </th>
              </tr>
            </thead>
            <tbody>
              {lines.map((l) => {
                const raw = draft.qty[l.id];
                const problem = lineProblem(l, raw);
                const removed = raw !== undefined && quantity(raw) === 0;
                const sent = Number(l.delivered_qty ?? 0);
                return (
                  <tr key={l.id} data-removed={removed || undefined}>
                    <td data-label={t("Item")}>
                      {isolate(l.item_name ?? "")}
                      {sent > 0 && <span className="faint"> · {t("{sent} shipped", { sent: units(sent) })}</span>}
                    </td>
                    <td className="num" data-label={t("Price")}>
                      {isolate(money(l.rate))}
                    </td>
                    <td className="num" data-label={t("Qty")}>
                      {charge(l) ? (
                        <span className="faint">{t("Priced again")}</span>
                      ) : (
                        <>
                          <input
                            className="input num cm-qty"
                            type="number"
                            inputMode="decimal"
                            min={sent}
                            step="any"
                            aria-label={t("Quantity of {name}", { name: l.item_name ?? "item" })}
                            aria-invalid={!!problem}
                            value={raw ?? String(Number(l.qty ?? 0))}
                            onChange={(ev) => setQty(l.id, ev.target.value)}
                          />
                          {problem && <div className="field-error">{isolate(problem)}</div>}
                        </>
                      )}
                    </td>
                    <td>
                      {!charge(l) &&
                        (removed ? (
                          <Button size="sm" onClick={() => setDraft((d) => ({ ...d, qty: Object.fromEntries(Object.entries(d.qty).filter(([k]) => k !== l.id)) }))}>
                            {t("Keep it")}
                          </Button>
                        ) : (
                          <Button size="sm" disabled={sent > 0} title={sent > 0 ? t("Part of this line has shipped") : undefined} aria-label={t("Remove {name}", { name: l.item_name ?? "item" })} onClick={() => setQty(l.id, "0")}>
                            {t("Remove")}
                          </Button>
                        ))}
                    </td>
                  </tr>
                );
              })}
              {draft.added.map((a) => (
                <tr key={a.key}>
                  <td data-label={t("Item")}>
                    <LinkInput to="Item" value={a.item} title={a.title} label={t("Item to add")} placeholder={t("Find an item to add")} onChange={(id, title) => setDraft((d) => ({ ...d, added: d.added.map((x) => (x.key === a.key ? { ...x, item: id, title: title ?? "" } : x)) }))} />
                  </td>
                  <td className="num faint" data-label={t("Price")}>
                    {t("Worked out when saved")}
                  </td>
                  <td className="num" data-label={t("Qty")}>
                    <input className="input num cm-qty" type="number" min={1} step="any" aria-label={t("How many to add")} value={a.qty} onChange={(ev) => setDraft((d) => ({ ...d, added: d.added.map((x) => (x.key === a.key ? { ...x, qty: ev.target.value } : x)) }))} />
                  </td>
                  <td>
                    <Button size="sm" onClick={() => setDraft((d) => ({ ...d, added: d.added.filter((x) => x.key !== a.key) }))}>
                      {t("Take away")}
                    </Button>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
          <div>
            <Button
              icon="plus"
              onClick={() => {
                setDraft((d) => ({ ...d, added: [...d.added, { key: next, item: null, title: "", qty: "1" }] }));
                setNext(next + 1);
              }}
            >
              {t("Add an item")}
            </Button>
          </div>
          <label className="check-field">
            <input type="checkbox" checked={!!draft.address} onChange={(ev) => setDraft((d) => ({ ...d, address: ev.target.checked ? emptyAddress() : null }))} />
            <span>{t("It goes to a different address")}</span>
          </label>
          {draft.address && <AddressFields a={draft.address} onChange={(address) => setDraft((d) => ({ ...d, address }))} />}
          <Field label={t("Note")} htmlFor="edit-note" hint={t("For your own records.")}>
            <textarea id="edit-note" className="textarea" rows={2} maxLength={2000} value={note} onChange={(ev) => setNote(ev.target.value)} />
          </Field>
          <label className="check-field">
            <input type="checkbox" checked={notify} onChange={(ev) => setNotify(ev.target.checked)} />
            <span>{t("Tell the shopper what changed")}</span>
          </label>
        </div>
        <aside className="cm-refund-side" aria-label={t("What the change comes to")}>
          {(needsRule || ((rules.data?.length ?? 0) > 1 && ready.ok)) && (
            <Field label={t("Delivery method")} htmlFor="edit-rule" hint={needsRule ? undefined : t("Left as it is, the order's own method is priced again for what it now holds and where it goes.")}>
              <select id="edit-rule" className="select" value={rule} onChange={(ev) => setRule(ev.target.value)}>
                <option value="">{t("As the order has it")}</option>
                {rules.data?.map((r) => (
                  <option key={r.id} value={r.id}>
                    {isolate(r.label ?? "")}
                  </option>
                ))}
              </select>
            </Field>
          )}
          {!ready.ok && ready.why ? (
            <p className="cm-refund-why" role="status">
              {isolate(ready.why)}
            </p>
          ) : refused ? (
            <Alert tone="error">{isolate(refused)}</Alert>
          ) : p ? (
            <>
              <Worked p={p} code={code} approve={approve} setApprove={setApprove} />
              {working && <p className="faint cm-tight">{t("Working it out again…")}</p>}
            </>
          ) : ready.ok && !held ? (
            <ListSkeleton rows={3} />
          ) : null}
        </aside>
      </div>
    </Dialog>
  );
}
