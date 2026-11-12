// The variants builder's rules, kept apart so they are tested without a browser: what the combinations are,
// which of them the store will not sell, the limits the server holds (src/variants.rs), and what is asked of
// it. The server makes the combinations in ERP, gives each its website item and price and leaves the
// disabled ones unpublished; this shows what it will make before it is asked.

import { t } from "@ferrum/client";

export const MAX_OPTIONS = 3;
export const MAX_VALUES = 30;
export const MAX_COMBINATIONS = 100;

/** An option and the values it comes in, in the order given. */
export interface Option {
  name: string;
  values: string[];
}

/** Values the store does not sell, by option name. */
export type Off = Record<string, string[]>;

const same = (a: string, b: string) => a.trim().toLowerCase() === b.trim().toLowerCase();

/** A value added to a list: trimmed, not empty, not there already in any case, at most 60 characters. */
export function addValue(values: string[], raw: string): { values: string[]; problem: string | null } {
  const v = raw.trim();
  if (!v) return { values, problem: null };
  if (v.length > 60) return { values, problem: t("A value is at most 60 characters.") };
  if (values.some((x) => same(x, v))) return { values, problem: t("{value} is in the list already.", { value: v }) };
  if (values.length >= MAX_VALUES) return { values, problem: t("At most {max} values to an option.", { max: MAX_VALUES }) };
  return { values: [...values, v], problem: null };
}

/** Every combination, the first option varying slowest; none when an option has no values. */
export function combinations(options: Option[]): string[][] {
  const named = options.filter((o) => o.name.trim());
  if (!named.length || named.some((o) => !o.values.length)) return [];
  return named.reduce<string[][]>((rows, o) => rows.flatMap((r) => o.values.map((v) => [...r, v])), [[]]);
}

export const isOff = (off: Off, option: string, value: string) => (off[Object.keys(off).find((k) => same(k, option)) ?? ""] ?? []).some((v) => same(v, value));

/** Whether a combination has a value the store does not sell: it is made, unpublished. */
export const comboOff = (options: Option[], combo: string[], off: Off) => options.filter((o) => o.name.trim()).some((o, i) => isOff(off, o.name, combo[i]));

/** What stops the options being generated, in words, or null. */
export function problem(options: Option[]): string | null {
  const named = options.filter((o) => o.name.trim());
  if (!named.length) return t("Name what it comes in, such as Size, and give its values.");
  if (named.length > MAX_OPTIONS) return t("At most {max} options.", { max: MAX_OPTIONS });
  if (named.some((o) => o.name.trim().length > 60)) return t("An option's name is at most 60 characters.");
  if (named.some((o, i) => named.findIndex((x) => same(x.name, o.name)) !== i)) return t("Each option is named once.");
  const empty = named.find((o) => !o.values.length);
  if (empty) return t("{name} needs at least one value.", { name: empty.name.trim() });
  const n = combinations(options).length;
  if (n > MAX_COMBINATIONS) return t("That makes {n} combinations: at most {max} at once.", { n, max: MAX_COMBINATIONS });
  return null;
}

/** The two parameters of Generate Variants, as JSON text. `disabled` is left out when nothing is off. */
export function params(options: Option[], off: Off): { options: string; disabled?: string } {
  const named = options.filter((o) => o.name.trim());
  const made = Object.fromEntries(named.map((o) => [o.name.trim(), o.values]));
  const not = Object.fromEntries(named.map((o) => [o.name.trim(), o.values.filter((v) => isOff(off, o.name, v))]).filter(([, v]) => (v as string[]).length));
  return { options: JSON.stringify(made), disabled: Object.keys(not).length ? JSON.stringify(not) : undefined };
}

/** What the server answers (src/variants.rs). */
export interface Made {
  variants_made_by_erp?: unknown;
  website_items_made?: number;
  unpublished?: number;
}

export function madeWords(r: Made): string {
  const n = r.website_items_made ?? 0;
  if (!n) return t("Nothing new to make: every combination was there already.");
  const off = r.unpublished ?? 0;
  return off ? t("Made {n} website items, {off} of them unpublished because they have a value the store does not sell. Each has the price of this item: set its own on its form.", { n, off }) : t("Made {n} website items. Each has the price of this item: set its own on its form.", { n });
}
