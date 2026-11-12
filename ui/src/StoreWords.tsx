// Storefront Words (/words): a workspace's own words for each language the store offers, over the English the app
// ships. One row per word, with the English beside the workspace's text; search, only the words not yet written,
// and reset a word to its default. About 165 words in a generic form are unusable; this is the same record
// (Storefront Words: one a language, a JSON object of key to text) written by hand-friendly rows. A word left
// empty is the English one, a language's missing words are English under it, and the server checks the words
// again and says so in its own if it refuses.

import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useEffect, useState } from "react";
import { api, docPath, errorMessage, isolate, t } from "@ferrum/client";
import { Alert, Button, confirmDialog, EmptyState, ErrorState, Field, ListSkeleton, StatePanel, toast, useDebounced, useShellTitle } from "@ferrum/kit";
import { useEntityMeta, useMe } from "@ferrum/records";
import english from "../../assets/words/en.json";
import { list, soft, useCanEditWords } from "./data";
import { Tabs } from "./Tabs";
import { firstProblem, kept, offered, problem, rows, same, written } from "./wordrules";

const EN = english as Record<string, string>;

interface Record_ {
  id: string;
  version?: number;
  language: string;
  words?: unknown;
}

/** A language's name in its own words, with its code. */
function languageName(code: string): string {
  try {
    const name = new Intl.DisplayNames([code], { type: "language" }).of(code);
    return name && name !== code ? `${name} (${code})` : code;
  } catch {
    return code;
  }
}

function Editor({ language, record, onSaved, onDirty }: { language: string; record: Record_ | undefined; onSaved: () => void; onDirty: (dirty: boolean) => void }) {
  // Only words the storefront uses: the server refuses any other, so one a release has dropped is left out.
  const start = Object.fromEntries(Object.entries(written(record?.words)).filter(([k]) => k in EN));
  const [draft, setDraft] = useState<Record<string, string>>(start);
  const [search, setSearch] = useState("");
  const [onlyMissing, setOnlyMissing] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const q = useDebounced(search, 200);
  const shown = rows(EN, draft, { search: q, onlyMissing });
  const total = Object.keys(EN).length;
  const own = Object.keys(kept(draft)).filter((k) => k in EN).length;
  const dirty = !same(draft, start);
  const bad = firstProblem(EN, draft);
  useEffect(() => onDirty(dirty), [dirty, onDirty]);

  const save = async () => {
    if (busy || bad || !dirty) return;
    setBusy(true);
    setError(null);
    try {
      const words = kept(draft);
      if (record) await api.patch(docPath("StorefrontWords", record.id), { words }, { ifMatch: record.version });
      else await api.post(docPath("StorefrontWords"), { language, words });
      toast(t("Saved. The storefront shows these words within a minute."));
      onSaved();
    } catch (err) {
      setError(errorMessage(err));
      setBusy(false);
    }
  };

  return (
    <div className="stack">
      {error && <Alert tone="error">{isolate(error)}</Alert>}
      <div className="cm-words-bar">
        <Field label={t("Search the words")} htmlFor="words-search">
          <input id="words-search" className="input" type="search" value={search} placeholder={t("A key, the English or your text")} autoComplete="off" onChange={(e) => setSearch(e.target.value)} />
        </Field>
        <label className="check-field">
          <input type="checkbox" checked={onlyMissing} onChange={(e) => setOnlyMissing(e.target.checked)} />
          <span>{t("Only the words not yet written")}</span>
        </label>
        <p className="faint cm-tight" role="status">
          {t("{own} of {total} words written for {language}. The rest show in English.", { own, total, language })}
        </p>
      </div>
      {shown.length === 0 ? (
        <EmptyState title={onlyMissing && !q ? t("Every word is written") : t("No word matches")} icon="check">
          {onlyMissing && !q ? t("Every word of the storefront has your text for this language.") : t("Change the search, or show every word.")}
        </EmptyState>
      ) : (
        <table className="table cm-table cm-words">
          <thead>
            <tr>
              <th>{t("Word")}</th>
              <th>{t("English")}</th>
              <th>{t("Your text")}</th>
              <th>
                <span className="sr-only">{t("Reset")}</span>
              </th>
            </tr>
          </thead>
          <tbody>
            {shown.map((r) => {
              const why = r.own ? problem(r.text, r.english) : null;
              const id = `word-${r.key}`;
              return (
                <tr key={r.key} data-own={r.own || undefined}>
                  <td data-label={t("Word")}>
                    <code className="mono">{r.key}</code>
                  </td>
                  <td data-label={t("English")}>{isolate(r.english)}</td>
                  <td data-label={t("Your text")}>
                    <textarea
                      id={id}
                      className="textarea"
                      rows={Math.min(5, Math.max(1, Math.ceil(r.english.length / 50)))}
                      value={r.text}
                      placeholder={r.english}
                      aria-label={t("Your text for {key}", { key: r.key })}
                      aria-invalid={!!why}
                      dir="auto"
                      onChange={(e) => setDraft((d) => ({ ...d, [r.key]: e.target.value }))}
                    />
                    {why && <div className="field-error">{isolate(why)}</div>}
                  </td>
                  <td>
                    <Button
                      size="sm"
                      disabled={!r.own}
                      aria-label={t("Reset {key} to its default", { key: r.key })}
                      onClick={() => setDraft((d) => Object.fromEntries(Object.entries(d).filter(([k]) => k !== r.key)))}
                    >
                      {t("Reset to default")}
                    </Button>
                  </td>
                </tr>
              );
            })}
          </tbody>
        </table>
      )}
      <div className="row cm-end-row">
        {bad && (
          <p className="cm-refund-why" role="status">
            {isolate(`${bad[0]}: ${bad[1]}`)}
          </p>
        )}
        <Button onClick={() => setDraft(start)} disabled={!dirty || busy}>
          {t("Discard changes")}
        </Button>
        <Button variant="primary" loading={busy} disabled={!dirty || !!bad} onClick={() => void save()}>
          {t("Save the words")}
        </Button>
      </div>
    </div>
  );
}

export function Words() {
  useShellTitle(t("Storefront words"));
  const qc = useQueryClient();
  const me = useMe();
  const meta = useEntityMeta("StorefrontWords");
  const allowed = useCanEditWords();
  const [chosen, setChosen] = useState<string | null>(null);
  const [dirty, setDirty] = useState(false);
  const settings = useQuery({ enabled: allowed, queryKey: ["commerce", "languages"], queryFn: ({ signal }) => list<{ store_languages?: string | null }>("StoreSettings", { fields: "store_languages", limit: 1 }, signal).then((r) => offered(r.rows[0]?.store_languages)).catch(() => ["en"]) });
  const records = useQuery({ enabled: allowed, queryKey: ["commerce", "words"], queryFn: ({ signal }) => soft(list<Record_>("StorefrontWords", { fields: "language,words", limit: 100 }, signal)) });

  if (me.isPending || meta.isPending) return <ListSkeleton rows={4} />;
  if (!allowed) return <StatePanel kind="denied">{t("The storefront's words are for the store's managers. Ask an administrator for the Website Manager role.")}</StatePanel>;
  // The languages the store offers, and any a record exists for.
  const languages = [...new Set([...(settings.data ?? []), ...(records.data ?? []).map((r) => r.language.toLowerCase())])];
  const language = chosen && languages.includes(chosen) ? chosen : (languages[0] ?? "en");
  const record = records.data?.find((r) => r.language.toLowerCase() === language);
  const refresh = () => void qc.invalidateQueries({ queryKey: ["commerce", "words"] });

  const pick = async (code: string) => {
    if (dirty && !(await confirmDialog({ title: t("Leave without saving?"), message: t("What you typed for this language is not saved."), confirmLabel: t("Leave") }))) return;
    setDirty(false);
    setChosen(code);
  };

  const body = records.error ? (
    <ErrorState error={records.error} onRetry={() => void records.refetch()} />
  ) : !records.data || !settings.data ? (
    <ListSkeleton rows={6} />
  ) : (
    // Keyed by the language and the saved version: a save, or another language, starts from what the server holds.
    <Editor key={`${language}:${record?.version ?? 0}`} language={language} record={record} onSaved={refresh} onDirty={setDirty} />
  );

  return (
    <div className="cm-page">
      <header className="cm-head">
        <div>
          <Tabs />
          <p className="page-sub">{t("The storefront's own words, language by language, over the English that comes with it.")}</p>
        </div>
      </header>
      <div className="cm-words-lang">
        <Field label={t("Language")} htmlFor="words-language" hint={t("The languages the store offers, from Store Settings' Languages Offered.")}>
          <select id="words-language" className="select" value={language} onChange={(e) => void pick(e.target.value)}>
            {languages.map((c) => (
              <option key={c} value={c}>
                {languageName(c)}
              </option>
            ))}
          </select>
        </Field>
      </div>
      {body}
    </div>
  );
}
