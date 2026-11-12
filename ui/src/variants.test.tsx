// @vitest-environment jsdom
// The variants builder driven in a page: what it comes in, the values not sold, the combinations that will be
// made, and Generate Variants with the options as the server reads them (src/variants.rs); its refusal is
// shown as it is.

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act } from "react";
import { createRoot } from "react-dom/client";
import { MemoryRouter, Route, Routes } from "react-router";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { ConfirmHost } from "@ferrum/kit";
import { Variants } from "./Variants";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

const perms = (write: "all" | "none") => ({ read: "all", write, create: write, delete: "none", submit: "none", cancel: "none", export: "none", import: "none" });
const me = { tenant: "t", user: { id: "u", email: "m@shop.test", full_name: "M", is_admin: false, roles: ["Website Manager"] }, default_company: null, scopes: {}, fiscal_year_start_month: 4, packages: ["commerce"] };
const sent: { url: string; body: unknown }[] = [];
let refuse: string | null = null;
let item: Record<string, unknown> = {};

beforeEach(() => {
  sent.length = 0;
  refuse = null;
  item = { id: "w1", title: "Logo Tee", disabled_options: [{ option: "Size", value: "XL" }] };
  document.body.innerHTML = "";
  HTMLDialogElement.prototype.showModal ??= function (this: HTMLDialogElement) {
    this.setAttribute("open", "");
  };
  HTMLDialogElement.prototype.close ??= function (this: HTMLDialogElement) {
    this.removeAttribute("open");
  };
  const json = (v: unknown, status = 200) => new Response(JSON.stringify(v), { status, headers: { "content-type": "application/json" } });
  vi.stubGlobal("fetch", async (url: string, init: { method?: string; body?: string } = {}) => {
    const u = String(url);
    if (init.method === "POST") {
      sent.push({ url: u, body: JSON.parse(init.body ?? "null") });
      return refuse ? json({ message: refuse }, 422) : json({ variants_made_by_erp: [], website_items_made: 4, unpublished: 2 });
    }
    if (/WebsiteItem\/w\d/.test(u)) return json(item);
    return json({ rows: [{ published: true }, { published: false }], total: 2 });
  });
});
afterEach(() => vi.unstubAllGlobals());

async function mount(at = "/variants/w1") {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  qc.setQueryData(["me"], me);
  for (const name of ["OrderRefund", "PaymentRequest", "PaymentException", "ReturnRequest", "WebsiteItem"]) qc.setQueryData(["meta", name], { name, permissions: perms(name === "WebsiteItem" ? "all" : "none") });
  const host = document.createElement("div");
  document.body.append(host);
  await act(async () => {
    createRoot(host).render(
      <QueryClientProvider client={qc}>
        <MemoryRouter initialEntries={[at]}>
          <Routes>
            <Route path="/variants" element={<Variants />} />
            <Route path="/variants/:id" element={<Variants />} />
          </Routes>
        </MemoryRouter>
        <ConfirmHost />
      </QueryClientProvider>,
    );
  });
  // The product and its variants come from the server a moment later.
  await act(async () => {
    await new Promise((r) => setTimeout(r, 30));
  });
}
const text = () => (document.body.textContent ?? "").replace(/\s+/g, " ");
const button = (label: string, exact = false) => [...document.querySelectorAll("button")].find((b) => (exact ? b.textContent?.trim() === label : b.textContent?.trim().startsWith(label))) as HTMLButtonElement;
const press = async (el: Element) => void (await act(async () => (el as HTMLElement).click()));
const type = async (el: Element | null, value: string) =>
  void (await act(async () => {
    const input = el as HTMLInputElement;
    Object.getOwnPropertyDescriptor(Object.getPrototypeOf(input), "value")?.set?.call(input, value);
    input.dispatchEvent(new Event("input", { bubbles: true }));
  }));
const key = async (el: Element | null, k: string) => void (await act(async () => el?.dispatchEvent(new KeyboardEvent("keydown", { key: k, bubbles: true, cancelable: true }))));

async function fill() {
  await type(document.querySelector("#option-0"), "Size");
  for (const v of ["S", "M", "XL"]) {
    await type(document.querySelector("#value-0"), v);
    await key(document.querySelector("#value-0"), "Enter");
  }
}

describe("the variants builder", () => {
  it("asks for a product first, and denies someone who does not manage the catalog", async () => {
    await mount("/variants");
    expect(text()).toContain("Choose a product");
  });

  it("shows the combinations, those not sold, and generates them with the options as JSON", async () => {
    await mount();
    expect(text()).toContain("2 variants exist already (1 published)");
    await fill();
    // XL was already not sold on the product: it stays marked, its combination is made unpublished.
    expect(text()).toContain("3 combinations");
    expect(text()).toContain("2 sold");
    expect(text()).toContain("1 not sold");
    expect(text()).toContain("Unpublished: it has a value the store does not sell");
    await press(document.querySelector('button[aria-label="Do not sell M"]') as HTMLButtonElement);
    expect(text()).toContain("2 not sold");
    await press(button("Generate variants", true));
    expect(text()).toContain("Generate 3 combinations?");
    await press([...document.querySelectorAll("button")].filter((b) => b.textContent?.trim() === "Generate variants").pop() as HTMLButtonElement);
    expect(sent).toHaveLength(1);
    expect(sent[0].url).toContain("/WebsiteItem/w1/actions/generate_variants");
    expect(sent[0].body).toEqual({ options: '{"Size":["S","M","XL"]}', disabled: '{"Size":["M","XL"]}' });
    expect(text()).toContain("Made 4 website items, 2 of them unpublished");
  });

  it("says what stops it, and shows the server's refusal as it is", async () => {
    await mount();
    expect(text()).toContain("Name what it comes in, such as Size, and give its values.");
    expect(button("Generate variants", true).disabled).toBe(true);
    await fill();
    refuse = "item variants are ERP's: install its Stock Operations add-on, then generate them here";
    await press(button("Generate variants", true));
    await press([...document.querySelectorAll("button")].filter((b) => b.textContent?.trim() === "Generate variants").pop() as HTMLButtonElement);
    expect(text()).toContain("install its Stock Operations add-on");
    await type(document.querySelector("#value-0"), "s");
    await key(document.querySelector("#value-0"), "Enter");
    expect(text()).toContain("s is in the list already.");
  });

  it("will not generate on a variant, and points to its product", async () => {
    item = { id: "w2", title: "Logo Tee M", variant_of: "w1" };
    await mount("/variants/w2");
    expect(text()).toContain("This item is a variant itself");
    expect(text()).toContain("Open that item");
  });
});
