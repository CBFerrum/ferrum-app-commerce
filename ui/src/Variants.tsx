// A product that comes in options: what it comes in (Size S, M, L; Colour Red, Blue), the values the store does
// not sell, and every combination that will be made, before "Generate Variants" is run. The server makes the
// combinations as ERP's item variants, each with its website item (a variant of this one, which the product
// page's picker reads) and the price of this item, and leaves the combinations with a value that is not sold
// unpublished; stock is per combination, as each is an item of its own.

import { useQuery, useQueryClient } from "@tanstack/react-query";
import { type FormEvent, useEffect, useState } from "react";
import { Link, useNavigate, useParams } from "react-router";
import { api, docPath, errorMessage, isolate, t } from "@ferrum/client";
import { Alert, Badge, Button, confirmDialog, EmptyState, ErrorState, Field, FormSkeleton, StatePanel, toast, useShellTitle } from "@ferrum/kit";
import { LinkInput, useEntityMeta, can } from "@ferrum/records";
import { f, list, useRights } from "./data";
import { addValue, combinations, comboOff, isOff, madeWords, type Made, MAX_COMBINATIONS, type Off, type Option, params, problem } from "./combos";
import { Tabs } from "./Tabs";

interface Item {
  id: string;
  title?: string | null;
  slug?: string | null;
  variant_of?: string | null;
  published?: boolean | null;
  disabled_options?: { option: string; value: string }[];
}

function Values({ option, off, onChange, onOff, index }: { option: Option; off: Off; onChange: (values: string[]) => void; onOff: (value: string) => void; index: number }) {
  const [typed, setTyped] = useState("");
  const [said, setSaid] = useState<string | null>(null);
  const add = () => {
    const r = addValue(option.values, typed);
    setSaid(r.problem);
    if (!r.problem) {
      onChange(r.values);
      setTyped("");
    }
  };
  return (
    <div className="stack">
      <ul className="cm-chips" aria-label={t("Values of {name}", { name: option.name || t("this option") })}>
        {option.values.map((v) => {
          const not = isOff(off, option.name, v);
          return (
            <li key={v} className="chip" data-off={not || undefined}>
              <span>{isolate(v)}</span>
              <button type="button" className="link-btn" aria-pressed={not} aria-label={not ? t("{value} is not sold: sell it", { value: v }) : t("Do not sell {value}", { value: v })} onClick={() => onOff(v)}>
                {not ? t("Not sold") : t("Sold")}
              </button>
              <button type="button" className="icon-btn icon-btn-sm" aria-label={t("Remove {value}", { value: v })} onClick={() => onChange(option.values.filter((x) => x !== v))}>
                ×
              </button>
            </li>
          );
        })}
      </ul>
      <div className="row">
        <input
          className="input"
          aria-label={t("Add a value to {name}", { name: option.name || t("this option") })}
          aria-invalid={!!said}
          id={`value-${index}`}
          value={typed}
          maxLength={60}
          placeholder={t("A value, then Enter")}
          onChange={(e) => setTyped(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" || e.key === ",") {
              e.preventDefault();
              add();
            }
          }}
        />
        <Button onClick={add}>{t("Add")}</Button>
      </div>
      {said && <div className="field-error">{isolate(said)}</div>}
    </div>
  );
}

function Builder({ id }: { id: string }) {
  const qc = useQueryClient();
  const q = useQuery({
    queryKey: ["commerce", "variants", id],
    queryFn: async ({ signal }) => {
      const [item, existing] = await Promise.all([api.get<Item>(docPath("WebsiteItem", id), undefined, signal), list<{ published?: boolean | null }>("WebsiteItem", { filters: f([["variant_of", "=", id]]), fields: "published", limit: 200 }, signal).then((r) => r.rows)]);
      return { item, existing };
    },
  });
  const [options, setOptions] = useState<Option[]>([{ name: "", values: [] }]);
  const [off, setOff] = useState<Off>({});
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [made, setMade] = useState<Made | null>(null);
  // What the store has already decided not to sell stays marked, as the server keeps it.
  useEffect(() => {
    const kept: Off = {};
    for (const d of q.data?.item.disabled_options ?? []) (kept[d.option] ??= []).push(d.value);
    setOff(kept);
  }, [q.data]);

  if (q.error) return <ErrorState error={q.error} onRetry={() => void q.refetch()} />;
  if (!q.data) return <FormSkeleton fields={3} />;
  const { item, existing } = q.data;
  if (item.variant_of) {
    return (
      <Alert tone="warning">
        {t("This item is a variant itself: generate variants on the item they belong to.")} <Link to={`/variants/${item.variant_of}`}>{t("Open that item")}</Link>
      </Alert>
    );
  }
  const stop = problem(options);
  const rows = combinations(options);
  const named = options.filter((o) => o.name.trim());
  const sold = rows.filter((r) => !comboOff(options, r, off)).length;
  const toggle = (name: string, value: string) =>
    setOff((o) => {
      const key = Object.keys(o).find((k) => k.toLowerCase() === name.trim().toLowerCase()) ?? name.trim();
      const has = isOff(o, name, value);
      return { ...o, [key]: has ? (o[key] ?? []).filter((v) => v.toLowerCase() !== value.toLowerCase()) : [...(o[key] ?? []), value] };
    });
  const set = (i: number, o: Partial<Option>) => setOptions((all) => all.map((x, j) => (j === i ? { ...x, ...o } : x)));

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    if (stop || busy) return;
    const ok = await confirmDialog({
      title: t("Generate {n} combinations?", { n: rows.length }),
      message: t("{sold} are sold and {off} are made unpublished. The missing ones are made as ERP's item variants, each with a website item and the price of this one.", { sold, off: rows.length - sold }),
      confirmLabel: t("Generate variants"),
    });
    if (!ok) return;
    setBusy(true);
    setError(null);
    try {
      const r = await api.post<Made>(docPath("WebsiteItem", id, "actions", "generate_variants"), params(options, off));
      setMade(r);
      toast(madeWords(r));
      void qc.invalidateQueries({ queryKey: ["commerce"] });
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setBusy(false);
    }
  };

  return (
    <form className="stack" onSubmit={submit} aria-label={t("Generate variants")}>
      {error && <Alert tone="error">{isolate(error)}</Alert>}
      {made && <Alert tone="success">{isolate(madeWords(made))}</Alert>}
      <p className="muted cm-tight">
        {existing.length ? t("{n} variants exist already ({published} published). Only the missing combinations are made.", { n: existing.length, published: existing.filter((v) => v.published).length }) : t("It has no variants yet.")}
      </p>
      {options.map((o, i) => (
        <fieldset key={i} className="card cm-option">
          <legend className="sr-only">{t("Option {n}", { n: i + 1 })}</legend>
          <Field label={t("What it comes in")} htmlFor={`option-${i}`} hint={i === 0 ? t("Such as Size or Colour.") : undefined}>
            <input id={`option-${i}`} className="input" value={o.name} maxLength={60} autoComplete="off" onChange={(e) => set(i, { name: e.target.value })} />
          </Field>
          <Values option={o} off={off} index={i} onChange={(values) => set(i, { values })} onOff={(v) => toggle(o.name, v)} />
          {options.length > 1 && (
            <Button size="sm" onClick={() => setOptions((all) => all.filter((_, j) => j !== i))}>
              {t("Remove this option")}
            </Button>
          )}
        </fieldset>
      ))}
      {options.length < 3 && (
        <div>
          <Button icon="plus" onClick={() => setOptions((all) => [...all, { name: "", values: [] }])}>
            {t("Another option")}
          </Button>
        </div>
      )}
      {stop ? (
        <p className="faint" role="status">
          {isolate(stop)}
        </p>
      ) : (
        <section aria-labelledby="combos">
          <h2 className="cm-group" id="combos">
            {t("{n} combinations", { n: rows.length })} <Badge tone="green">{t("{n} sold", { n: sold })}</Badge> {rows.length - sold > 0 && <Badge tone="neutral">{t("{n} not sold", { n: rows.length - sold })}</Badge>}
          </h2>
          <table className="table cm-table cm-matrix">
            <thead>
              <tr>
                {named.map((o) => (
                  <th key={o.name}>{isolate(o.name)}</th>
                ))}
                <th>{t("Made as")}</th>
              </tr>
            </thead>
            <tbody>
              {rows.map((r) => {
                const not = comboOff(options, r, off);
                return (
                  <tr key={r.join("\u0000")}>
                    {r.map((v, i) => (
                      <td key={i} data-label={named[i]?.name} data-off={isOff(off, named[i].name, v) || undefined}>
                        {isolate(v)}
                      </td>
                    ))}
                    <td data-label={t("Made as")}>{not ? t("Unpublished: it has a value the store does not sell") : t("Published, with the price of this item")}</td>
                  </tr>
                );
              })}
            </tbody>
          </table>
          <p className="faint">{t("At most {max} combinations at once. Stock is kept per combination: each is an item of its own.", { max: MAX_COMBINATIONS })}</p>
        </section>
      )}
      <div className="row cm-end-row">
        <Link to={`/e/WebsiteItem/${id}`} className="btn btn-default btn-md">
          {t("Back to the item")}
        </Link>
        <Button type="submit" variant="primary" loading={busy} disabled={!!stop}>
          {t("Generate variants")}
        </Button>
      </div>
    </form>
  );
}

export function Variants() {
  useShellTitle(t("Variants"));
  const { id } = useParams();
  const nav = useNavigate();
  const rights = useRights();
  const meta = useEntityMeta("WebsiteItem");
  if (!rights.ready || meta.isPending) return <FormSkeleton fields={3} />;
  if (!can(meta.data?.permissions, "write")) return <StatePanel kind="denied">{t("Variants are made by the store's managers. Ask an administrator for the Website Manager role.")}</StatePanel>;
  return (
    <div className="cm-page">
      <header className="cm-head">
        <div>
          <Tabs />
          <p className="page-sub">{t("Make a product's sizes and colours as combinations, each with its own price and stock.")}</p>
        </div>
      </header>
      <div className="stack">
        <Field label={t("Product")} htmlFor="variants-item">
          <LinkInput to="WebsiteItem" id="variants-item" value={id ?? null} label={t("Product")} placeholder={t("Find a product")} onChange={(v) => nav(v ? `/variants/${v}` : "/variants")} />
        </Field>
        {id ? (
          <Builder key={id} id={id} />
        ) : (
          <EmptyState title={t("Choose a product")} icon="file">
            {t("A size or a colour is an item of its own. Choose the product, say what it comes in, and the combinations are made for you.")}
          </EmptyState>
        )}
      </div>
    </div>
  );
}
