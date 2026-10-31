// The store's figures (kept apart, so they are tested): web sales over the last days from the
// Web Sales report's daily rows.

export interface DayRow {
  day: string;
  orders: number | string;
  revenue: number | string;
}

export interface Totals {
  orders: number;
  revenue: number;
}

/** A day `n` days before `today` (YYYY-MM-DD). */
export function daysBefore(today: string, n: number): string {
  const t = Date.parse(`${today}T00:00:00Z`) - n * 86_400_000;
  return new Date(t).toISOString().slice(0, 10);
}

/** Orders and revenue on the days from `from` to `to` (inclusive). */
export function totals(rows: DayRow[], from: string, to: string): Totals {
  return rows
    .filter((r) => r.day >= from && r.day <= to)
    .reduce((t, r) => ({ orders: t.orders + Number(r.orders || 0), revenue: t.revenue + Number(r.revenue || 0) }), { orders: 0, revenue: 0 });
}

/** Money as people read it, without the cents of large sums. */
export function money(n: number): string {
  return n.toLocaleString(undefined, { maximumFractionDigits: Math.abs(n) >= 1000 ? 0 : 2 });
}
