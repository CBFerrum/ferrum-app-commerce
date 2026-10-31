import { describe, expect, it } from "vitest";
import { daysBefore, totals } from "./store";

describe("the store's figures", () => {
  it("adds up the days asked for", () => {
    const rows = [
      { day: "2026-10-25", orders: 2, revenue: "100.50" },
      { day: "2026-10-31", orders: "3", revenue: "300" },
      { day: "2026-11-01", orders: 1, revenue: 49.5 },
    ];
    expect(totals(rows, "2026-11-01", "2026-11-01")).toEqual({ orders: 1, revenue: 49.5 });
    expect(totals(rows, daysBefore("2026-11-01", 6), "2026-11-01")).toEqual({ orders: 4, revenue: 349.5 });
    expect(totals(rows, daysBefore("2026-11-01", 29), "2026-11-01")).toEqual({ orders: 6, revenue: 450 });
    expect(totals([], "2026-11-01", "2026-11-01")).toEqual({ orders: 0, revenue: 0 });
  });

  it("counts back across months", () => {
    expect(daysBefore("2026-11-01", 1)).toBe("2026-10-31");
    expect(daysBefore("2026-03-01", 1)).toBe("2026-02-28");
  });
});
