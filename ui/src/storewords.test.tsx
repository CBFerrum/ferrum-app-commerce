// @vitest-environment jsdom
// Storefront Words, driven in a page: the languages the store offers, a word typed and saved as the record takes
// it (src/words.rs), search, only the words not yet written, reset to default, and the server's refusal as it is.

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act } from "react";
import { createRoot } from "react-dom/client";
import { renderToStaticMarkup } from "react-dom/server";
import { MemoryRouter } from "react-router";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { Words } from "./StoreWords";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

const perms = (write: "all" | "none") => ({ read: "all", write, create: write, delete: "none", submit: "none", cancel: "none", export: "none", import: "none" });
const me = { tenant: "t", user: { id: "u", email: "m@shop.test", full_name: "M", is_admin: false, roles: ["Website Manager"] }, default_company: null, scopes: {}, fiscal_year_start_month: 4, packages: ["commerce"] };

const sent: { method: string; url: string; body: unknown; ifMatch: string | null }[] = [];
let refuse: string | null = null;
let rowsOf: unknown[] = [];

beforeEach(() => {
  sent.length = 0;
  refuse = null;
  rowsOf = [];
  document.body.innerHTML = "";
  vi.stubGlobal("fetch", async (url: string, init: { method?: string; body?: string; headers?: Record<string, string> } = {}) => {
    const json = (v: unknown, status = 200) => new Response(JSON.stringify(v), { status, headers: { "content-type": "application/json" } });
    if (init.method && init.method !== "GET") {
      const headers = new Headers(init.headers);
      sent.push({ method: init.method, url: String(url), body: init.body ? JSON.parse(init.body) : null, ifMatch: headers.get("if-match") });
      return refuse ? json({ message: refuse, fields: [{ field: "words", message: refuse }] }, 422) : json({ id: "w1" });
    }
    if (String(url).includes("/StoreSettings")) return json({ rows: [{ store_languages: "en, hi" }], total: 1 });
    return json({ rows: rowsOf, total: rowsOf.length });
  });
});
afterEach(() => vi.unstubAllGlobals());

function client(write: "all" | "none" = "all") {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: Infinity, gcTime: Infinity } } });
  qc.setQueryData(["me"], me);
  qc.setQueryData(["meta", "StorefrontWords"], { name: "StorefrontWords", permissions: perms(write) });
  return qc;
}

async function mount(qc = client()) {
  const host = document.createElement("div");
  document.body.append(host);
  await act(async () => {
    createRoot(host).render(
      <QueryClientProvider client={qc}>
        <MemoryRouter>
          <Words />
        </MemoryRouter>
      </QueryClientProvider>,
    );
  });
  await act(async () => void (await new Promise((r) => setTimeout(r, 20))));
}
const text = () => (document.body.textContent ?? "").replace(/\s+/g, " ");
const press = async (el: Element | null) => void (await act(async () => (el as HTMLElement).click()));
const type = async (el: Element | null, value: string) =>
  void (await act(async () => {
    const input = el as HTMLTextAreaElement | HTMLInputElement;
    Object.getOwnPropertyDescriptor(Object.getPrototypeOf(input), "value")?.set?.call(input, value);
    input.dispatchEvent(new Event("input", { bubbles: true }));
  }));
const button = (label: string) => [...document.querySelectorAll("button")].find((b) => b.textContent?.trim().startsWith(label)) ?? null;
const box = (key: string) => document.querySelector(`textarea[aria-label="Your text for ${key}"]`);
const wait = () => act(async () => void (await new Promise((r) => setTimeout(r, 260))));

describe("the storefront's words", () => {
  it("lists the languages the store offers and every word with its English, and writes a word as the record takes it", async () => {
    await mount();
    expect(text()).toContain("Storefront words");
    const select = document.querySelector("#words-language") as HTMLSelectElement;
    expect([...select.options].map((o) => o.value)).toEqual(["en", "hi"]);
    expect(text()).toContain("Your cart is empty");
    expect(text()).toMatch(/0 of \d+ words written for en/);
    expect((button("Save the words") as HTMLButtonElement).disabled).toBe(true);
    await type(box("cart.empty"), "Aapki tokri khaali hai");
    expect(text()).toMatch(/1 of \d+ words written/);
    await press(button("Save the words"));
    // A new language is a new record: the language and the words written, nothing else.
    expect(sent).toEqual([{ method: "POST", url: "/api/v1/StorefrontWords", body: { language: "en", words: { "cart.empty": "Aapki tokri khaali hai" } }, ifMatch: null }]);
  });

  it("changes the record that is there, with its version, and resets a word to its default", async () => {
    rowsOf = [{ id: "w1", version: 3, language: "en", words: { "cart.empty": "Khaali", "cart.checkout": "Aage", "gone.key": "x" } }];
    await mount();
    expect((box("cart.empty") as HTMLTextAreaElement).value).toBe("Khaali");
    await press(document.querySelector('button[aria-label="Reset cart.empty to its default"]'));
    expect((box("cart.empty") as HTMLTextAreaElement).value).toBe("");
    await press(button("Save the words"));
    // The word reset is gone; a key the storefront no longer uses is not sent back.
    expect(sent[0]).toMatchObject({ method: "PATCH", url: "/api/v1/StorefrontWords/w1", body: { words: { "cart.checkout": "Aage" } }, ifMatch: "\"v3\"" });
  });

  it("searches, and shows only the words not yet written", async () => {
    rowsOf = [{ id: "w1", version: 1, language: "en", words: { "cart.empty": "Khaali" } }];
    await mount();
    const all = document.querySelectorAll("textarea").length;
    expect(all).toBeGreaterThan(100);
    await press(document.querySelector('input[type="checkbox"]'));
    expect(document.querySelectorAll("textarea").length).toBe(all - 1);
    expect(box("cart.empty")).toBeNull();
    await type(document.querySelector("#words-search"), "zzzzzz");
    await wait();
    expect(text()).toContain("No word matches");
    await type(document.querySelector("#words-search"), "your cart is empty");
    await wait();
    expect(document.querySelectorAll("textarea").length).toBe(0);
    await press(document.querySelector('input[type="checkbox"]'));
    await wait();
    expect(box("cart.empty")).not.toBeNull();
  });

  it("will not save a place its English does not fill, and shows the server's refusal as it is", async () => {
    await mount();
    await type(box("cart.count_aria"), "{count} mein");
    expect(text()).toContain("{count} is not filled here: its English has no such place.");
    expect((button("Save the words") as HTMLButtonElement).disabled).toBe(true);
    await type(box("cart.count_aria"), "{n} mein");
    refuse = "cart.count_aria names {n}, which its English does not fill";
    await press(button("Save the words"));
    expect(text()).toContain("cart.count_aria names {n}, which its English does not fill");
    expect((button("Save the words") as HTMLButtonElement).disabled).toBe(false);
  });

  it("is for those who may change the words, and shows loading before they are there", () => {
    const denied = renderToStaticMarkup(
      <QueryClientProvider client={client("none")}>
        <MemoryRouter>
          <Words />
        </MemoryRouter>
      </QueryClientProvider>,
    );
    expect(denied).toContain("are for the store&#x27;s managers");
    const loading = renderToStaticMarkup(
      <QueryClientProvider client={new QueryClient()}>
        <MemoryRouter>
          <Words />
        </MemoryRouter>
      </QueryClientProvider>,
    );
    expect(loading).toContain('aria-label="Loading…"');
  });
});
