import { describe, expect, it } from "vitest";
import { addValue, combinations, comboOff, madeWords, params, problem } from "./combos";

const sizes = { name: "Size", values: ["S", "M", "L"] };
const colours = { name: "Colour", values: ["Red", "Blue"] };

describe("the combinations", () => {
  it("are every value with every other, the first option varying slowest", () => {
    expect(combinations([sizes, colours]).map((c) => c.join("/"))).toEqual(["S/Red", "S/Blue", "M/Red", "M/Blue", "L/Red", "L/Blue"]);
    expect(combinations([sizes, { name: "Colour", values: [] }])).toEqual([]);
    expect(combinations([])).toEqual([]);
  });

  it("leave unpublished those with a value the store does not sell, whatever its case", () => {
    const off = { size: ["l"] };
    expect(comboOff([sizes, colours], ["L", "Red"], off)).toBe(true);
    expect(comboOff([sizes, colours], ["M", "Red"], off)).toBe(false);
  });
});

describe("the values and the limits", () => {
  it("takes a value once, trimmed, and says why it did not", () => {
    expect(addValue(["S"], "  M ")).toEqual({ values: ["S", "M"], problem: null });
    expect(addValue(["S"], "s").problem).toMatch(/in the list already/);
    expect(addValue(["S"], "   ")).toEqual({ values: ["S"], problem: null });
    expect(addValue(Array.from({ length: 30 }, (_, i) => `v${i}`), "x").problem).toMatch(/At most 30/);
    expect(addValue([], "x".repeat(61)).problem).toMatch(/60 characters/);
  });

  it("holds the server's limits before it is asked", () => {
    expect(problem([{ name: "", values: [] }])).toMatch(/Name what it comes in/);
    expect(problem([sizes, { name: "size", values: ["x"] }])).toMatch(/named once/);
    expect(problem([sizes, { name: "Colour", values: [] }])).toBe("Colour needs at least one value.");
    expect(problem([sizes, colours, { name: "Fit", values: ["Slim"] }, { name: "Era", values: ["New"] }])).toMatch(/At most 3 options/);
    const many = (n: string) => ({ name: n, values: Array.from({ length: 11 }, (_, i) => `v${i}`) });
    expect(problem([many("A"), many("B")])).toMatch(/121 combinations: at most 100/);
    expect(problem([sizes, colours])).toBeNull();
  });
});

describe("what is asked of the server", () => {
  it("is JSON text of the options, and of what is not sold when something is not", () => {
    expect(params([sizes, colours], {})).toEqual({ options: '{"Size":["S","M","L"],"Colour":["Red","Blue"]}', disabled: undefined });
    expect(params([sizes, colours], { Size: ["L"] })).toEqual({ options: '{"Size":["S","M","L"],"Colour":["Red","Blue"]}', disabled: '{"Size":["L"]}' });
  });

  it("says what was made from the server's answer", () => {
    expect(madeWords({ website_items_made: 0 })).toMatch(/Nothing new/);
    expect(madeWords({ website_items_made: 6, unpublished: 2 })).toContain("Made 6 website items, 2 of them unpublished");
  });
});
