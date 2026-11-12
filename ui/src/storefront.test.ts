// @vitest-environment jsdom
// The storefront's script (assets/store.js) run against a page and a store that answer as the server's
// routes do (request and answer shapes are those of src/site.rs, src/cart.rs, src/orders.rs and
// src/catalog.rs): the cart's place and codes, the checkout's card and consent, an order whose payment
// failed, an order changed after it was paid, and the product page's picker and back-in-stock box.

import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

/** The storefront's English words, which the server writes into every page (src/pages.rs). */
const WORDS = readFileSync(resolve(process.cwd(), "../assets/words/en.json"), "utf8");

type Body = Record<string, unknown> | null;
type Headers = Record<string, string>;
type Answer = unknown | ((body: Body, headers: Headers) => unknown);
const calls: { method: string; path: string; body: Record<string, unknown> | null; headers: Record<string, string> }[] = [];

/** A fetch that answers by "METHOD /path"; a thrown {status, body} is a refusal. */
function serve(routes: Record<string, Answer>) {
  calls.length = 0;
  vi.stubGlobal("fetch", async (url: string, init: { method?: string; body?: string; headers?: Record<string, string> } = {}) => {
    const path = String(url).replace("/api/site/t/commerce", "");
    const method = init.method ?? "GET";
    const body = init.body ? (JSON.parse(init.body) as Record<string, unknown>) : null;
    calls.push({ method, path, body, headers: init.headers ?? {} });
    const answer = routes[`${method} ${path}`];
    if (answer === undefined) return { ok: false, status: 404, json: async () => ({ message: `no route ${method} ${path}` }) };
    const value = typeof answer === "function" ? (answer as (b: unknown, h: unknown) => unknown)(body, init.headers ?? {}) : answer;
    if (value && typeof value === "object" && "refuse" in value) {
      const r = value as { refuse: number; body: unknown };
      return { ok: false, status: r.refuse, json: async () => r.body };
    }
    return { ok: true, status: 200, json: async () => value };
  });
}

async function open(path: string, body: string, view?: string, data: Record<string, string> = {}, words: Record<string, string> = {}, lang = "en") {
  window.history.pushState({}, "", path);
  document.documentElement.lang = lang;
  // What the server writes in: English, with the page's language over it.
  const page = JSON.stringify({ ...JSON.parse(WORDS), ...words });
  document.body.innerHTML = `${view ? `<span data-cart-count hidden></span><div id="view" data-view="${view}" aria-busy="true"></div>` : ""}${body}<div id="live"></div><script type="application/json" id="store-words">${page}</script>`;
  Object.assign(document.body.dataset, { api: "/api/site/t/commerce", currency: "INR", precision: "2", tax: "false", ...data });
  vi.resetModules();
  // The script is the storefront's own file, not one of this interface's: it carries no types.
  // @ts-expect-error a plain script
  const mod = (await import("../../assets/store.js")) as { started: Promise<unknown> };
  await mod.started;
  await new Promise((r) => setTimeout(r, 0));
  return mod;
}
const text = () => (document.body.textContent ?? "").replace(/\s+/g, " ");
const tick = () => new Promise((r) => setTimeout(r, 0));

beforeEach(() => {
  // jsdom draws nothing, so it has nothing to scroll.
  Element.prototype.scrollIntoView = () => {};
  localStorage.clear();
  sessionStorage.clear();
});
afterEach(() => vi.unstubAllGlobals());

const line = { id: "l1", item: "tee", title: "Logo Tee", qty: "1", price: "400", amount: "400", available: true, in_stock: true };
const totals = (over: Record<string, unknown> = {}) => ({ subtotal: "400", discount: "0", shipping: "0", taxes: [], total_taxes: "0", grand_total: "400", rounding_adjustment: "0", rounded_total: "400", ...over });
const cart = (over: Record<string, unknown> = {}) => ({ status: "Open", currency: "INR", items: [line], item_count: "1", codes: [], promotions: [], shipping: null, shipping_address: {}, totals: totals(), missing: ["email"], ...over });
const store = { name: "Shop", gift_cards: true, countries: [{ code: "IN", name: "India" }, { code: "DE", name: "Germany" }], payment_methods: [{ gateway: "stripe", label: "Card", online: true }, { gateway: "Cash on Delivery", label: "Cash on delivery", online: false }], allow_guest_checkout: true, terms_url: null };

describe("the cart", () => {
  it("takes where it goes and shows the delivery methods and the total for that place", async () => {
    localStorage.setItem("ferrum.cart:/api/site/t/commerce", JSON.stringify("tok"));
    let placed: Record<string, unknown> | null = null;
    serve({
      "GET /cart": () => (placed ? cart({ shipping_address: placed, shipping: { id: "r1", label: "Standard", amount: "50" }, totals: totals({ shipping: "50", taxes: [{ description: "GST 18%", amount: "72" }], rounded_total: "522", grand_total: "522" }) }) : cart()),
      "GET /store": store,
      "PATCH /cart/details": (b: Body) => ((placed = (b as { shipping_address: Record<string, unknown> }).shipping_address), cart({ shipping_address: placed, missing: ["shipping_method"] })),
      "GET /cart/shipping-options": () => ({ options: [{ id: "r1", label: "Standard", amount: "50" }, { id: "r2", label: "Express", amount: "0", description: "Free over 500" }], selected: null }),
      "PUT /cart/shipping": () => cart({ shipping_address: placed, shipping: { id: "r1", label: "Standard", amount: "50" }, totals: totals({ shipping: "50", taxes: [{ description: "GST 18%", amount: "72" }], rounded_total: "522", grand_total: "522" }) }),
    });
    await open("/cart", "", "cart");
    expect(text()).toContain("Where is it going?");
    expect(text()).toContain("Tax and delivery are not in this total yet");
    const form = document.querySelector("form.place") as HTMLFormElement;
    (form.elements.namedItem("country") as HTMLSelectElement).value = "IN";
    (form.elements.namedItem("state") as HTMLInputElement).value = "Karnataka";
    (form.elements.namedItem("postal_code") as HTMLInputElement).value = "560001";
    form.dispatchEvent(new Event("submit", { cancelable: true }));
    await tick();
    await tick();
    await tick();
    expect(calls.find((c) => c.method === "PATCH")?.body).toEqual({ shipping_address: { country: "IN", state: "Karnataka", postal_code: "560001" } });
    expect(text()).toContain("Standard · INR 50.00");
    expect(text()).toContain("Express · Free");
    expect(text()).toContain("GST 18%INR 72.00");
    expect(text()).toContain("TotalINR 522.00");
    expect(text()).toContain("Tax and delivery for Karnataka, India");
  });

  it("says plainly when nothing delivers to the place, and keeps the refusal of an unknown country", async () => {
    localStorage.setItem("ferrum.cart:/api/site/t/commerce", JSON.stringify("tok"));
    serve({
      "GET /cart": cart({ shipping_address: { country: "DE", state: null, postal_code: null }, missing: ["shipping_method"] }),
      "GET /store": store,
      "GET /cart/shipping-options": { options: [], selected: null },
      "PATCH /cart/details": { refuse: 422, body: { message: "invalid", fields: [{ field: "shipping_address.country", message: "we do not ship to this country" }] } },
    });
    await open("/cart", "", "cart");
    expect(text()).toContain("We cannot deliver to that place");
    const form = document.querySelector("form.place") as HTMLFormElement;
    form.dispatchEvent(new Event("submit", { cancelable: true }));
    await tick();
    await tick();
    expect(text()).toContain("we do not ship to this country");
  });

  it("lists every code with what it took, and one that does not apply", async () => {
    localStorage.setItem("ferrum.cart:/api/site/t/commerce", JSON.stringify("tok"));
    serve({
      "GET /cart": cart({ codes: [{ code: "SPRING", applied: true }, { code: "OLD", applied: false }], promotions: [{ title: "Spring sale", amount: "40" }] }),
      "GET /store": store,
      "DELETE /cart/codes/OLD": cart({ codes: [{ code: "SPRING", applied: true }], promotions: [{ title: "Spring sale", amount: "40" }] }),
    });
    await open("/cart", "", "cart");
    expect(text()).toContain("SPRING applied");
    expect(text()).toContain("OLD is on the cart, but it does not apply");
    expect(text()).toContain("Spring sale: −INR 40.00");
    (document.querySelector('button[aria-label="Remove code OLD"]') as HTMLButtonElement).click();
    await tick();
    await tick();
    expect(calls.some((c) => c.method === "DELETE" && c.path === "/cart/codes/OLD")).toBe(true);
    expect(text()).not.toContain("OLD is on the cart");
  });
});

describe("the checkout", () => {
  const placedCart = cart({ email: "a@x.test", name: "Asha Rao", shipping_address: { line1: "12 MG Road", city: "Bengaluru", state: "KA", postal_code: "560001", country: "IN" }, billing_same_as_shipping: true, missing: [] });
  const base = {
    "GET /cart": placedCart,
    "GET /store": store,
    "PATCH /cart/details": placedCart,
    "GET /cart/shipping-options": { options: [{ id: "r1", label: "Standard", amount: "50" }], selected: "r1" },
    "PUT /cart/shipping": placedCart,
  };

  async function toPayment(routes: Record<string, Answer>) {
    localStorage.setItem("ferrum.cart:/api/site/t/commerce", JSON.stringify("tok"));
    serve({ ...base, ...routes });
    await open("/checkout", "", "checkout");
    const details = document.querySelector("form.panel") as HTMLFormElement;
    (details.elements.namedItem("reminder_consent") as HTMLInputElement).checked = true;
    details.dispatchEvent(new Event("submit", { cancelable: true }));
    await tick();
    await tick();
    await tick();
  }

  it("sends the reminder consent with the details, and asks for the card with what happens to it", async () => {
    await toPayment({});
    expect(calls.filter((c) => c.method === "PATCH").pop()?.body?.reminder_consent).toBe(true);
    expect(text()).toContain("Gift card or store credit");
    expect(text()).toContain("What if the card holds less than the total?");
    expect(text()).toContain("held on the card for this order, and nothing is charged until the rest is paid");
    expect(text()).toContain("Delivery to KA, India");
  });

  it("places the order once with the card's code and the other method, and goes to the order page", async () => {
    const assign = vi.fn();
    vi.stubGlobal("location", { ...window.location, assign, replace: vi.fn(), origin: "http://localhost:3000", pathname: "/checkout", search: "", hash: "" });
    await toPayment({ "POST /checkout": (_b: Body, h: Headers) => ({ order: { number: "SO-1", email: "a@x.test" }, order_token: "t1", payment_request: null, key: h["idempotency-key"] }) });
    const form = [...document.querySelectorAll("form.panel")].pop() as HTMLFormElement;
    (form.elements.namedItem("gift_card.code") as HTMLInputElement).value = "GC-1";
    (form.elements.namedItem("gift_card.pin") as HTMLInputElement).value = "1234";
    form.dispatchEvent(new Event("submit", { cancelable: true }));
    form.dispatchEvent(new Event("submit", { cancelable: true })); // a double press
    await tick();
    await tick();
    const sent = calls.filter((c) => c.path === "/checkout");
    expect(sent).toHaveLength(1);
    expect(sent[0].body).toMatchObject({ gift_card: { code: "GC-1", pin: "1234" }, payment_method: "stripe", expected_total: "400" });
    expect(sent[0].headers["idempotency-key"]).toMatch(/.{8,}/);
    expect(assign).toHaveBeenCalledWith("/order");
  });

  it("puts the card's refusal next to the card, and the order is not placed", async () => {
    await toPayment({ "POST /checkout": { refuse: 422, body: { message: "invalid", fields: [{ field: "gift_card", message: "this card holds nothing to pay with" }] } } });
    const form = [...document.querySelectorAll("form.panel")].pop() as HTMLFormElement;
    (form.elements.namedItem("gift_card.code") as HTMLInputElement).value = "GC-0";
    form.dispatchEvent(new Event("submit", { cancelable: true }));
    await tick();
    await tick();
    expect(text()).toContain("this card holds nothing to pay with");
    expect((form.elements.namedItem("gift_card.code") as HTMLInputElement).getAttribute("aria-invalid")).toBe("true");
    expect((form.querySelector("button[type=submit]") as HTMLButtonElement).disabled).toBe(false);
  });
});

describe("an order", () => {
  const link = { number: "SO-1", token: "tok" };
  const order = (over: Record<string, unknown> = {}) => ({
    number: "SO-1",
    status: "To Deliver and Bill",
    payment_status: "Pending",
    email: null,
    currency: "INR",
    steps: [{ label: "Order placed", done: true }, { label: "Payment received", done: false }, { label: "Shipped", done: false }],
    items: [{ item: "tee", title: "Logo Tee", qty: "1", price: "400", amount: "400" }],
    shipping: { method: "Standard", amount: "50" },
    totals: { subtotal: "400", shipping: "50", taxes: [], rounded_total: "450", grand_total: "450" },
    shipments: [],
    refunds: [],
    payments: [],
    ship_to: "Bengaluru · IN",
    ...over,
  });
  const track = (o: unknown) => ({ "POST /orders/track": { order: o }, "POST /orders/lookup": { order: o } });
  const on = (o: Record<string, unknown> = {}) => open(`/order#n=${link.number}&t=${link.token}`, "", "order").then(() => o);

  it("offers a way to try again when the payment failed, and asks for the email the link does not hold", async () => {
    const failed = { id: "p1", status: "Failed", gateway: "stripe", amount: "450", currency: "INR", label: "Card" };
    serve({ ...track(order({ payments: [failed] })), "GET /store": store, "POST /orders/pay": () => ({ payment_request: { id: "p2", status: "Initiated", payment_url: "https://pay.example/x" } }) });
    const assign = vi.fn();
    vi.stubGlobal("location", { ...window.location, assign, origin: "http://localhost:3000", pathname: "/order", search: "", hash: `#n=${link.number}&t=${link.token}`, reload: vi.fn() });
    await on();
    await tick();
    expect(text()).toContain("Your payment did not go through");
    expect(text()).toContain("Try again · INR 450.00");
    const form = document.querySelector(".retry form") as HTMLFormElement;
    form.dispatchEvent(new Event("submit", { cancelable: true }));
    await tick();
    expect(text()).toContain("Enter the email you ordered with");
    expect(calls.some((c) => c.path === "/orders/pay")).toBe(false);
    (form.elements.namedItem("email") as HTMLInputElement).value = "a@x.test";
    form.dispatchEvent(new Event("submit", { cancelable: true }));
    await tick();
    await tick();
    expect(calls.find((c) => c.path === "/orders/pay")?.body).toMatchObject({ number: "SO-1", email: "a@x.test", token: "tok", payment_method: "stripe" });
    expect(assign).toHaveBeenCalledWith("https://pay.example/x");
  });

  it("asks for the difference on an order changed after it was paid, and says what is on its way back", async () => {
    const paid = { id: "p1", status: "Paid", gateway: "stripe", amount: "450", currency: "INR" };
    const asked = { id: "p2", status: "Initiated", gateway: "stripe", amount: "120", currency: "INR", payment_url: "https://pay.example/d" };
    serve({ ...track(order({ payment_status: "Paid", payments: [asked, paid], totals: { subtotal: "520", shipping: "50", taxes: [], rounded_total: "570", grand_total: "570" }, refunds: [{ status: "Approved", kind: "Adjustment", amount: "30", currency: "INR" }] })) });
    await on();
    expect(text()).toContain("Your order was changed after you paid. You have paid INR 450.00; it now comes to INR 570.00, so INR 120.00 more is due.");
    expect(text()).toContain("Pay the difference · INR 120.00");
    expect(text()).toContain("INR 30.00 on its way back to you because your order was changed");
    expect(document.querySelector(".retry")).toBeNull();
  });

  it("explains a gift card's held part, and a card that paid", async () => {
    serve({ ...track(order({ payments: [{ id: "p2", status: "Initiated", gateway: "stripe", amount: "300", currency: "INR", payment_url: "https://pay.example/h" }] })) });
    await on();
    expect(text()).toContain("Your gift card or store credit is holding INR 150.00 for this order.");
    expect(text()).toContain("the hold is let go, your card is untouched and the order is cancelled");
    serve({ ...track(order({ payments: [{ id: "c1", status: "Paid", gateway: "Gift Card", amount: "150", currency: "INR" }, { id: "p2", status: "Initiated", gateway: "stripe", amount: "300", currency: "INR", payment_url: "https://pay.example/h" }] })) });
    await on();
    expect(text()).toContain("A gift card or store credit paid INR 150.00 of this order. The other INR 300.00 is still to pay.");
    // What the server says of the card: what it has left, and when a held part lets go.
    serve({ ...track(order({ gift_card: { paid: "0", held: "150", held_until: "2026-11-16T10:00:00.000Z", balance_left: "20" }, payments: [{ id: "p2", status: "Initiated", gateway: "stripe", amount: "300", currency: "INR", payment_url: "https://pay.example/h" }] })) });
    await on();
    expect(text()).toContain("INR 20.00 is left on the card.");
    expect(text()).toContain("The part held lets go on");
  });
});

describe("the product page", () => {
  const product = `<article class="product"><div class="buy"><h1>Tee</h1><p class="out">Out of stock</p>
    <fieldset class="option"><legend>Size: <strong>M</strong></legend><div class="picks"><a class="pick on" href="/p/tee?v=tee-m-red">M</a><a class="pick" href="/p/tee?v=tee-l-blue">L</a><a class="pick" href="/p/tee?v=tee-s-red">S</a></div></fieldset>
    <fieldset class="option"><legend>Colour: <strong>Red</strong></legend><div class="picks"><a class="pick on" href="/p/tee?v=tee-m-red">Red</a><a class="pick" href="/p/tee?v=tee-m-blue">Blue</a></div></fieldset></div></article>`;
  const v = (slug: string, size: string, colour: string, over: Record<string, unknown> = {}) => ({ slug, options: [{ option: "Size", value: size }, { option: "Colour", value: colour }], price: { currency: "INR", price: "400", price_with_tax: "472" }, purchasable: true, in_stock: true, available_qty: "3", ...over });
  const item = { slug: "tee", variants: [v("tee-m-red", "M", "Red", { in_stock: false, available_qty: "0" }), v("tee-m-blue", "M", "Blue", { price: { currency: "INR", price: "450" } }), v("tee-l-blue", "L", "Blue"), v("tee-s-red", "S", "Red", { purchasable: false })] };

  it("shows each combination as available, sold out or not offered, with price and stock", async () => {
    serve({ "GET /items/tee": item, "POST /items/tee-m-red/stock-alert": () => ({ status: "Waiting" }) });
    await open("/p/tee?v=tee-m-red", product);
    const pick = (label: string) => [...document.querySelectorAll(".pick")].find((a) => a.querySelector(".pick-value")?.textContent === label && a.closest("fieldset")?.querySelector("legend")?.textContent?.startsWith(label === "Blue" || label === "Red" ? "Colour" : "Size")) as HTMLElement;
    expect(pick("Blue").classList.contains("off")).toBe(false);
    expect(pick("Blue").textContent).toContain("3 in stock");
    expect(pick("Blue").title).toBe("3 in stock · INR 450.00");
    expect(pick("L").classList.contains("na")).toBe(true);
    expect(pick("L").textContent).toContain("Not offered with Red");
    expect(pick("S").classList.contains("off")).toBe(true);
    expect(pick("S").textContent).toContain("Not for sale");
    expect(pick("Red").classList.contains("off")).toBe(true);
    expect(pick("Red").textContent).toContain("Sold out");
    expect(text()).toContain("M / Red: INR 400.00 · Sold out");
  });

  it("takes an email for the variant that is out of stock, once, and shows the server's refusal", async () => {
    let refuse = false;
    serve({ "GET /items/tee": item, "POST /items/tee-m-red/stock-alert": () => (refuse ? { refuse: 422, body: { message: "this item is in stock: you can order it now" } } : { status: "Waiting" }) });
    await open("/p/tee?v=tee-m-red", product);
    const form = document.querySelector("form.alert-box") as HTMLFormElement;
    expect(text()).toContain("Leave your email and we will write once, when it is back.");
    (form.elements.namedItem("email") as HTMLInputElement).value = "nope";
    form.dispatchEvent(new Event("submit", { cancelable: true }));
    await tick();
    expect(text()).toContain("Enter an email address");
    (form.elements.namedItem("email") as HTMLInputElement).value = "a@x.test";
    form.dispatchEvent(new Event("submit", { cancelable: true }));
    await tick();
    await tick();
    expect(calls.find((c) => c.path === "/items/tee-m-red/stock-alert")?.body).toEqual({ email: "a@x.test" });
    expect(text()).toContain("We will write to you once, when it is back.");
    refuse = true;
    (form.elements.namedItem("email") as HTMLInputElement).value = "a@x.test";
    form.dispatchEvent(new Event("submit", { cancelable: true }));
    await tick();
    await tick();
    expect(text()).toContain("this item is in stock: you can order it now");
  });

  it("has no back-in-stock box where the item can be bought", async () => {
    serve({});
    await open("/p/mug", `<article class="product"><div class="buy"><p class="in">In stock</p><form class="add" data-item="mug"></form></div></article>`);
    expect(document.querySelector("form.alert-box")).toBeNull();
  });
});

describe("the storefront's words", () => {
  it("are the page's own, key by key, with their places filled and a count worded as the language counts", async () => {
    localStorage.setItem("ferrum.cart:/api/site/t/commerce", JSON.stringify("tok"));
    serve({ "GET /cart": () => cart({ item_count: "2", items: [line, { ...line, id: "l2", title: "Mug" }] }), "GET /store": store, "GET /cart/shipping-options": () => ({ options: [], selected: null }) });
    await open("/cart", "", "cart", {}, { "place.title": "Wohin geht es?", "totals.subtotal": "Zwischensumme", "line.removed": "{item} entfernt", "cart.items.other": "{n} Artikel" }, "de");
    expect(text()).toContain("Wohin geht es?");
    expect(text()).toContain("Zwischensumme");
    // A word the language has not is the English one.
    expect(text()).toContain("Checkout");
    expect(document.documentElement.lang).toBe("de");
    // A count is worded as the language counts (here the badge's own phrase, with {n} filled).
    expect(document.querySelector("[data-cart-count]")?.getAttribute("aria-label")).toBe("2 in your cart");
  });
});
