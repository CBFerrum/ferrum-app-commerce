// Storefront Words, kept apart so they are tested without a browser: which words a language has written, which
// rows a search and the "only the missing" choice leave, and what is wrong with a text before the server says it
// (src/words.rs: a word may only fill the places its English fills, and is at most 600 characters).

import { t } from "@ferrum/client";

export const MAX_LENGTH = 600;

/** The places a text fills (`{name}`), as the server counts them. */
export function places(text: string): Set<string> {
  return new Set([...text.matchAll(/\{(\w+)\}/g)].map((m) => m[1]));
}

/** What is wrong with a text for a word, in words, or null. The server checks the same and says the same. */
export function problem(text: string, english: string): string | null {
  if ([...text].length > MAX_LENGTH) return t("At most {max} characters.", { max: MAX_LENGTH });
  const extra = [...places(text)].find((p) => !places(english).has(p));
  return extra ? t("{place} is not filled here: its English has no such place.", { place: `{${extra}}` }) : null;
}

/** A language's words as the record holds them: an object of key to text (the answer may carry it as JSON text). Anything else is nothing. */
export function written(value: unknown): Record<string, string> {
  let v = value;
  if (typeof v === "string") {
    try {
      v = JSON.parse(v);
    } catch {
      return {};
    }
  }
  if (!v || typeof v !== "object" || Array.isArray(v)) return {};
  return Object.fromEntries(Object.entries(v as Record<string, unknown>).filter(([, x]) => typeof x === "string") as [string, string][]);
}

/** The words to keep: those with text. A text of nothing but spaces is not one. */
export const kept = (draft: Record<string, string>): Record<string, string> => Object.fromEntries(Object.entries(draft).filter(([, x]) => x.trim() !== ""));

export interface Row {
  key: string;
  english: string;
  text: string;
  /** The workspace has its own text for it. */
  own: boolean;
}

/** The rows to show: every English word, with the workspace's text; narrowed by what was searched (the key, the English or the text) and, if asked, to those not yet written. */
export function rows(english: Record<string, string>, draft: Record<string, string>, o: { search: string; onlyMissing: boolean }): Row[] {
  const q = o.search.trim().toLowerCase();
  return Object.entries(english)
    .map(([key, en]): Row => ({ key, english: en, text: draft[key] ?? "", own: (draft[key] ?? "").trim() !== "" }))
    .filter((r) => (!o.onlyMissing || !r.own) && (!q || [r.key, r.english, r.text].some((x) => x.toLowerCase().includes(q))));
}

/** Whether two sets of words are the same. */
export const same = (a: Record<string, string>, b: Record<string, string>): boolean => {
  const [x, y] = [kept(a), kept(b)];
  const keys = Object.keys(x);
  return keys.length === Object.keys(y).length && keys.every((k) => x[k] === y[k]);
};

/** The first word with something wrong, as [key, why], else null. */
export function firstProblem(english: Record<string, string>, draft: Record<string, string>): [string, string] | null {
  for (const [key, text] of Object.entries(kept(draft))) {
    const why = problem(text, english[key] ?? "");
    if (why) return [key, why];
  }
  return null;
}

/** A language code as the store lists it (src/words.rs `valid_code`). */
export function validCode(code: string): boolean {
  const parts = code.split("-");
  return !!code && code.length <= 16 && parts.every((p) => /^[a-z0-9]{1,8}$/i.test(p)) && /^[a-z]{2,8}$/i.test(parts[0]);
}

/** The languages Store Settings offers: codes separated by commas, lower case, once each; none given, English (src/words.rs `offered`). */
export function offered(setting: string | null | undefined): string[] {
  const out: string[] = [];
  for (const c of (setting ?? "").split(/[,; ]/).map((x) => x.trim().toLowerCase()).filter(validCode)) if (!out.includes(c)) out.push(c);
  return out.length ? out : ["en"];
}
