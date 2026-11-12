import { describe, expect, it } from "vitest";
import { firstProblem, kept, offered, places, problem, rows, same, validCode, written } from "./wordrules";

const en = { "cart.empty": "Your cart is empty", "cart.count_aria": "{n} in your cart", "checkout.place": "Place order · {total}" };

describe("what a word may say", () => {
  it("fills only the places its English fills, and is not too long (src/words.rs, problem)", () => {
    expect([...places("{n} in {total}")]).toEqual(["n", "total"]);
    expect(problem("{n} mein", en["cart.count_aria"])).toBeNull();
    expect(problem("Khaali", en["cart.empty"])).toBeNull();
    expect(problem("{count} in", en["cart.count_aria"])).toBe("{count} is not filled here: its English has no such place.");
    expect(problem("x".repeat(601), en["cart.empty"])).toBe("At most 600 characters.");
    expect(problem("x".repeat(600), en["cart.empty"])).toBeNull();
    expect(firstProblem(en, { "cart.empty": "ok", "checkout.place": "{sum}" })).toEqual(["checkout.place", "{sum} is not filled here: its English has no such place."]);
    expect(firstProblem(en, { "cart.empty": "  " })).toBeNull();
  });
});

describe("a language's words", () => {
  it("reads what the record holds, as an object or as its JSON text, and nothing else", () => {
    expect(written({ a: "x", b: 3 })).toEqual({ a: "x" });
    expect(written('{"a":"x"}')).toEqual({ a: "x" });
    expect(written("not json")).toEqual({});
    expect(written(null)).toEqual({});
    expect(written(["a"])).toEqual({});
  });

  it("keeps the words that have text, and compares two sets without the empty ones", () => {
    expect(kept({ a: "x", b: "", c: "  " })).toEqual({ a: "x" });
    expect(same({ a: "x", b: "" }, { a: "x" })).toBe(true);
    expect(same({ a: "x" }, { a: "y" })).toBe(false);
  });

  it("lists every English word with the workspace's text, narrowed by search and by what is not yet written", () => {
    const draft = { "cart.empty": "Khaali" };
    expect(rows(en, draft, { search: "", onlyMissing: false }).map((r) => [r.key, r.own])).toEqual([["cart.empty", true], ["cart.count_aria", false], ["checkout.place", false]]);
    expect(rows(en, draft, { search: "", onlyMissing: true }).map((r) => r.key)).toEqual(["cart.count_aria", "checkout.place"]);
    // By key, by English or by the workspace's own text, in any case.
    expect(rows(en, draft, { search: "CHECKOUT", onlyMissing: false }).map((r) => r.key)).toEqual(["checkout.place"]);
    expect(rows(en, draft, { search: "place order", onlyMissing: false }).map((r) => r.key)).toEqual(["checkout.place"]);
    expect(rows(en, draft, { search: "khaali", onlyMissing: false }).map((r) => r.key)).toEqual(["cart.empty"]);
    expect(rows(en, draft, { search: "khaali", onlyMissing: true })).toEqual([]);
  });

  it("knows a language code as the store lists it, and the languages Store Settings offers", () => {
    expect(validCode("pt-br") && validCode("en") && !validCode("e") && !validCode("en_US") && !validCode("1n") && !validCode("en-")).toBe(true);
    expect(offered(undefined)).toEqual(["en"]);
    expect(offered("hi, EN; ar hi, x, de-AT")).toEqual(["hi", "en", "ar", "de-at"]);
  });
});
