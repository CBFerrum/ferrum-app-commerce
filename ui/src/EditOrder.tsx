// Changing an order that has not been fulfilled: quantities, lines added and taken away, and where it
// goes. What it will come to is shown before anything is saved, with what happens to the difference
// (the shopper is asked for it, it is sent back, or it is recorded as owed); the server prices the
// change again and its own figures are said once it is saved. Nothing about the rules lives here: ERP
// refuses what has shipped or been billed, and the server's refusal is shown as it is.

import { useState } from "react";
import { api, docPath, errorMessage, isolate, t } from "@ferrum/client";
import { Alert, Button, Dialog, Field, toast } from "@ferrum/kit";
import { LinkInput } from "@ferrum/records";
import { type Address, changes, type Draft, emptyAddress, emptyDraft, estimate, type OrderLine, type PayRow, quantity, readiness, savedWords, type Saved, settlement, isCharge, lineProblem, updateParams } from "./edit";
import { amountText, units } from "./queues";
import { gatewayName } from "./standing";

interface Order {
  id: string;
  number?: string | null;
  commerce_card_held?: string | number | null;
}

/** What the difference comes to, in words, for the person about to save it. */
function Outcome({ difference, payments, code, approve, setApprove }: { difference: number; payments: PayRow[]; code?: string | null; approve: boolean; setApprove: (v: boolean) => void }) {
  const s = settlement(difference, payments);
  const money = (v: unknown) => amountText(v, code);
  const via = (g: string | null) => (g ? gatewayName(g) : t("the store's first payment method"));
  const words =
    s.kind === "none"
      ? t("Nothing is asked for and nothing is sent back.")
      : s.kind === "replace"
        ? t("The shopper has not paid yet: their open payment request is replaced by one for the new total, through {via}.", { via: via(s.via) })
        : s.kind === "charge"
          ? t("The shopper is asked for the difference through {via}, with a payment link on their order page.", { via: via(s.via) })
          : s.kind === "refund"
            ? s.offline
              ? t("The difference is noted as a refund through {via}, which has no gateway: you hand it back and record that you did.", { via: via(s.via) })
              : t("The difference is sent back through {via} as an adjustment refund.", { via: via(s.via) })
            : s.toShopper
              ? t("No payment can take a refund of this size, so it is recorded as owed to the shopper: the store owes {amount}.", { amount: money(Math.abs(difference)) })
              : t("No payment can be asked for it, so it is recorded as owed by the shopper: {amount} is still to collect.", { amount: money(Math.abs(difference)) });
  return (
    <div className="stack">
      <p className="cm-tight" role="status">
        {isolate(words)}
      </p>
      {s.kind === "refund" && !s.offline && (
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

export function EditOrderDialog({ order, lines, payments, shippingItem, code, onClose, onDone }: { order: Order; lines: OrderLine[]; payments: PayRow[]; shippingItem?: string | null; code?: string | null; onClose: () => void; onDone: () => void }) {
  const [draft, setDraft] = useState<Draft>(emptyDraft);
  const [note, setNote] = useState("");
  const [notify, setNotify] = useState(true);
  const [approve, setApprove] = useState(true);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [next, setNext] = useState(1);
  const held = Number(order.commerce_card_held ?? 0) > 0;
  const charge = (l: OrderLine) => isCharge(l, shippingItem);
  const ready = readiness(lines, draft, charge);
  const e = estimate(lines, draft);
  const money = (v: unknown) => amountText(v, code);
  const before = lines.filter((l) => !charge(l)).reduce((n, l) => n + Number(l.amount ?? 0), 0);

  const setQty = (id: string, v: string) => setDraft((d) => ({ ...d, qty: { ...d.qty, [id]: v } }));

  const submit = async () => {
    if (!ready.ok || busy) return;
    setBusy(true);
    setError(null);
    try {
      const saved = await api.post<Saved>(docPath("SalesOrder", order.id, "actions", "web_update_items"), updateParams(lines, draft, note, notify, approve));
      toast(savedWords(saved, money));
      onDone();
    } catch (err) {
      setError(errorMessage(err));
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
          <Button variant="primary" loading={busy} disabled={!ready.ok || held} onClick={() => void submit()}>
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
          <div className="cm-refund-sum" aria-live="polite">
            <span className="muted">{t("The lines change by about")}</span>
            <strong className="cm-refund-amount">{e.goods === 0 && !e.added && !e.moves ? "—" : isolate(`${e.goods > 0 ? "+" : ""}${money(e.goods)}`)}</strong>
            <span className="faint">{t("Before it was {amount}, before tax and delivery.", { amount: money(before) })}</span>
          </div>
          {(e.added > 0 || e.moves) && (
            <p className="faint cm-tight">
              {[e.added > 0 ? t("Added items are priced when it is saved.") : null, e.moves ? t("Tax follows the new address.") : null].filter(Boolean).join(" ")}
            </p>
          )}
          <p className="faint cm-tight">{t("Promotions, tax and delivery are priced again when it is saved: the exact difference is shown then.")}</p>
          {changes(lines, draft).length > 0 || e.moves ? <Outcome difference={e.goods} payments={payments} code={code} approve={approve} setApprove={setApprove} /> : null}
          {!ready.ok && ready.why && (
            <p className="cm-refund-why" role="status">
              {isolate(ready.why)}
            </p>
          )}
        </aside>
      </div>
    </Dialog>
  );
}
