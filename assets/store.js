// The storefront's script (src/pages.rs): the cart's count in the header, "Add to cart", the product
// page's pictures, and the cart, checkout and order pages, over the storefront API the page names in
// <body data-api>. Everything it shows from the API is set as text, never as markup. Every view has
// its loading, empty and failed states, and says what it does to a screen reader.

const page = document.body.dataset;
const API = page.api;
const CURRENCY = page.currency ?? "";
const PRECISION = Number(page.precision ?? 2);
const RETURN = page.return ?? null;
const CART = `ferrum.cart:${API}`;
const ORDER = `ferrum.order:${API}`;

const saved = {
  get(k) {
    try {
      return JSON.parse(localStorage.getItem(k) ?? "null");
    } catch {
      return null;
    }
  },
  set(k, v) {
    try {
      if (v == null) localStorage.removeItem(k);
      else localStorage.setItem(k, JSON.stringify(v));
    } catch {}
  },
};

const LABELS = { email: "Email", name: "Name", phone: "Phone", shipping_address: "Delivery address", billing_address: "Billing address", qty: "Quantity", code: "Code", gift_card: "Gift card", country: "Country" };

class ApiError extends Error {
  constructor(status, data) {
    const f = data?.fields?.[0];
    const where = f?.field ? `${LABELS[String(f.field).split(".")[0]] ?? f.field}: ` : "";
    super(f?.message ? `${where}${f.message}` : (data?.message ?? `Something went wrong (${status}). Try again.`));
    this.status = status;
    this.field = f?.field ?? null;
    this.detail = f?.message ?? null;
  }
}

async function call(method, path, body, headers = {}) {
  const token = saved.get(CART);
  let r;
  try {
    r = await fetch(`${API}${path}`, {
      method,
      credentials: "omit",
      headers: { accept: "application/json", ...(body ? { "content-type": "application/json" } : {}), ...(token ? { "x-cart-token": token } : {}), ...headers },
      body: body ? JSON.stringify(body) : undefined,
    });
  } catch {
    throw new ApiError(0, { message: "The store cannot be reached. Check your connection and try again." });
  }
  const data = await r.json().catch(() => null);
  if (!r.ok) throw new ApiError(r.status, data);
  return data;
}

const nf = new Intl.NumberFormat("en-US", { minimumFractionDigits: PRECISION, maximumFractionDigits: PRECISION });
const money = (v, code = CURRENCY) => `${code} ${nf.format(Number(v ?? 0))}`;

/** A country's name in the shopper's own language, from its two-letter code; the code when the browser does not know it. */
function countryName(code, countries = []) {
  if (!code) return "";
  const listed = countries.find((c) => c.code === code)?.name;
  if (listed) return listed;
  try {
    return new Intl.DisplayNames(undefined, { type: "region" }).of(code) ?? code;
  } catch {
    return code;
  }
}

/** "Bengaluru, Karnataka, India": where tax and delivery are worked out for; empty before a country is given. */
function placeText(cart, countries) {
  const a = cart.shipping_address ?? {};
  return a.country ? [a.state, countryName(a.country, countries)].filter(Boolean).join(", ") : "";
}

function h(tag, attrs = {}, ...kids) {
  const e = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs)) {
    if (v == null || v === false) continue;
    if (k.startsWith("on")) e.addEventListener(k.slice(2), v);
    else if (k === "text") e.textContent = v;
    else e.setAttribute(k, v === true ? "" : String(v));
  }
  e.append(...kids.flat().filter((c) => c != null && c !== false));
  return e;
}

function picture(img, size) {
  const url = img?.url;
  const ok = typeof url === "string" && (url.startsWith("/files/public/") || url.startsWith("https://"));
  return ok ? h("img", { src: url, alt: img.alt ?? "", width: size, height: size, loading: "lazy" }) : h("span", { class: "img none", "aria-hidden": "true" });
}

/** Says something to a screen reader without moving anyone's focus. */
function announce(message) {
  const live = document.getElementById("live");
  if (!live) return;
  live.textContent = "";
  // A change of text is what is read: clear first, so the same words twice are heard twice.
  setTimeout(() => (live.textContent = message), 30);
}

function skeleton() {
  return h("div", { class: "skeleton", "aria-hidden": "true" }, h("span"), h("span"), h("span"));
}

/** A view that could not be shown: what happened, and a way on. */
function failed(root, error, retry) {
  root.removeAttribute("aria-busy");
  root.replaceChildren(
    h(
      "div",
      { class: "state", role: "alert" },
      h("h2", { text: error.status === 404 ? "We could not find that" : "Something went wrong" }),
      h("p", { class: "muted", text: error.message }),
      h("div", { class: "row" }, retry ? h("button", { type: "button", text: "Try again", onclick: retry }) : null, h("a", { class: "button quiet", href: "/", text: "Go to the store" })),
    ),
  );
}

function ready(root) {
  root.removeAttribute("aria-busy");
}

// ───────────────────────── the cart ─────────────────────────

/** The cart, or null when there is none (never made, expired, or ordered). */
async function currentCart() {
  if (!saved.get(CART)) return null;
  try {
    const cart = await call("GET", "/cart");
    if (cart.status === "Open") return cart;
  } catch (e) {
    if (![400, 404].includes(e.status)) throw e;
  }
  saved.set(CART, null);
  return null;
}

async function addToCart(item, qty) {
  if (saved.get(CART)) {
    try {
      const cart = await call("POST", "/cart/items", { item, qty: String(qty) });
      if (cart.status === "Open") return cart;
    } catch (e) {
      if (![400, 404].includes(e.status)) throw e;
    }
    saved.set(CART, null);
  }
  const made = await call("POST", "/cart", { items: [{ item, qty: String(qty) }] });
  saved.set(CART, made.token);
  return made.cart;
}

function showCount(cart) {
  const badge = document.querySelector("[data-cart-count]");
  if (!badge) return;
  const n = cart ? Number(cart.item_count ?? 0) : 0;
  badge.hidden = !n;
  badge.textContent = n ? String(n) : "";
  badge.setAttribute("aria-label", n ? `${n} in your cart` : "");
}

function totals(cart, countries = [], estimate = true) {
  const t = cart.totals ?? {};
  const rows = [["Subtotal", t.subtotal]];
  if (Number(t.discount)) rows.push(["Discount", -Number(t.discount)]);
  if (cart.shipping) rows.push([`Delivery (${cart.shipping.label ?? "shipping"})`, t.shipping]);
  for (const tax of t.taxes ?? []) rows.push([tax.description ?? "Tax", tax.amount]);
  if (Number(t.rounding_adjustment)) rows.push(["Rounding", t.rounding_adjustment]);
  const box = h("dl", { class: "totals" });
  for (const [label, amount] of rows) box.append(h("dt", { text: label }), h("dd", { text: money(amount, cart.currency) }));
  box.append(h("dt", { class: "grand", text: "Total" }), h("dd", { class: "grand", text: money(t.rounded_total ?? t.grand_total, cart.currency) }));
  if (cart.prices_include_tax) box.append(h("dt", { class: "muted", text: "Prices include tax" }), h("dd"));
  // What the total is for: the place its tax and delivery were worked out for, or that it does not have them yet.
  if (estimate) {
    const where = placeText(cart, countries);
    box.append(h("dt", { class: "muted place-note", text: where ? `Tax and delivery for ${where}` : "Tax and delivery are not in this total yet: say where it goes" }), h("dd"));
  }
  return box;
}

/** A message at the top of a view, for a screen reader too; null clears it. */
function flash(root, message) {
  root.querySelector(":scope > .error[role=alert]")?.remove();
  if (!message) return;
  const box = h("p", { class: "error", role: "alert", tabindex: "-1", text: message });
  root.prepend(box);
  box.scrollIntoView({ block: "nearest" });
}

/** A cart line's quantity: minus, the number, plus. */
function stepper(l, onChange) {
  const input = h("input", { type: "number", min: "0", step: "any", inputmode: "decimal", value: l.qty, "aria-label": `Quantity of ${l.title ?? "item"}` });
  const set = (n) => {
    input.value = String(Math.max(0, n));
    onChange(input.value || "0");
  };
  input.addEventListener("change", () => onChange(input.value || "0"));
  return h(
    "div",
    { class: "stepper" },
    h("button", { type: "button", class: "quiet", text: "−", "aria-label": `One fewer ${l.title ?? "item"}`, onclick: () => set(Number(input.value || 0) - 1) }),
    input,
    h("button", { type: "button", class: "quiet", text: "+", "aria-label": `One more ${l.title ?? "item"}`, onclick: () => set(Number(input.value || 0) + 1) }),
  );
}

/** The delivery methods for where the cart goes, with the first chosen when none is: the cart as it then stands, and the methods (null before a country is known). */
async function delivery(cart) {
  if (!cart.shipping_address?.country) return { cart, options: null };
  const { options = [], selected = null } = await call("GET", "/cart/shipping-options");
  if (options.length && !options.some((o) => o.id === selected)) cart = await call("PUT", "/cart/shipping", { shipping_method: options[0].id });
  return { cart, options };
}

/** Where it goes: country, region and postal code, so tax and delivery are worked out for the place and the total shown is the total charged. */
function placePanel(cart, countries, act) {
  const a = cart.shipping_address ?? {};
  const country = countries.length
    ? h("label", {}, "Country", h("select", { name: "country", autocomplete: "country" }, h("option", { value: "", text: "Choose…" }), countries.map((c) => h("option", { value: c.code, selected: a.country === c.code, text: c.name ?? c.code }))))
    : field("Country (two letters, such as IN)", "country", a.country, { maxlength: "2", autocomplete: "country" });
  const form = h(
    "form",
    { class: "panel place", "aria-labelledby": "place-title", novalidate: true },
    h("h2", { id: "place-title", text: "Where is it going?" }),
    h("p", { class: "muted", text: "Tax and delivery depend on the place. Tell us now and the total below is the total you pay." }),
    h("div", { class: "fields" }, country, field("State or region", "state", a.state, { autocomplete: "address-level1" }), field("Postal code", "postal_code", a.postal_code, { autocomplete: "postal-code" })),
    h("p", {}, h("button", { class: "quiet", type: "submit", text: a.country ? "Update delivery and total" : "Show delivery and total" })),
  );
  form.addEventListener("submit", (ev) => {
    ev.preventDefault();
    clear(form);
    const f = form.elements;
    if (!f.country.value.trim()) return void fieldError(form, { field: "country", detail: "Choose where it goes" });
    const next = { ...a, country: f.country.value.trim().toUpperCase(), state: f.state.value.trim() || null, postal_code: f.postal_code.value.trim() || null };
    act(async () => {
      const done = await delivery(await call("PATCH", "/cart/details", { shipping_address: next }));
      return done;
    }, "Delivery and total worked out for the place you gave");
  });
  return form;
}

/** The delivery methods that reach the place, with what each costs (free when it is), or why there are none. */
function deliveryPanel(cart, options, act) {
  if (!options) return null;
  if (!options.length) {
    return (cart.missing ?? []).includes("shipping_method")
      ? h("p", { class: "notice error", role: "status", text: "We cannot deliver to that place. Change the country, region or postal code, or contact the store." })
      : null;
  }
  return h(
    "fieldset",
    { class: "panel" },
    h("legend", { text: "Delivery" }),
    options.map((o) =>
      h(
        "label",
        { class: "choice" },
        h("input", { type: "radio", name: "ship", value: o.id, checked: o.id === cart.shipping?.id, onchange: (ev) => act(() => call("PUT", "/cart/shipping", { shipping_method: ev.target.value }).then((c) => ({ cart: c, options })), `Delivery: ${o.label ?? "chosen"}`) }),
        h("span", {}, h("strong", { text: o.label ?? "Delivery" }), ` · ${Number(o.amount) === 0 ? "Free" : money(o.amount, cart.currency)}`, o.description ? h("div", { class: "muted", text: o.description }) : null),
      ),
    ),
  );
}

/** Promotion codes (several) and the store's own coupon (one): what each took off, and which do not apply. */
function codesPanel(cart, act) {
  const codes = cart.codes ?? [];
  const row = (code, words, remove) =>
    h("li", {}, h("strong", { text: code }), ` ${words} `, h("button", { class: "link", type: "button", text: "Remove", "aria-label": `Remove code ${code}`, onclick: () => act(remove, `Code ${code} removed`) }));
  const input = h("input", { name: "code", placeholder: "Promotion or discount code", "aria-label": "Promotion or discount code", maxlength: "60", autocomplete: "off" });
  const form = h(
    "form",
    { class: "coupon", onsubmit: (ev) => (ev.preventDefault(), input.value.trim() && act(() => call("PUT", "/cart/coupon", { code: input.value.trim() }), "Code applied")) },
    input,
    h("button", { class: "quiet", type: "submit", text: "Apply" }),
  );
  const took = (cart.promotions ?? []).filter((p) => Number(p.amount));
  return h(
    "div",
    { class: "codes" },
    h("ul", { class: "applied" }, cart.coupon ? row(cart.coupon.code, "applied", () => call("DELETE", "/cart/coupon")) : null, codes.map((c) => row(c.code, c.applied ? "applied" : "is on the cart, but it does not apply to what is in it", () => call("DELETE", `/cart/codes/${encodeURIComponent(c.code)}`)))),
    took.length ? h("ul", { class: "took" }, took.map((p) => h("li", { class: "muted", text: `${p.title}: −${money(p.amount, cart.currency)}` }))) : null,
    form,
  );
}

async function cartView(root) {
  const params = new URLSearchParams(location.search);
  let problem = null;
  if (params.get("add")) {
    // "Add to cart" before its script had loaded.
    try {
      await addToCart(params.get("add"), params.get("qty") || "1");
    } catch (e) {
      problem = e.message;
    }
    history.replaceState(null, "", "/cart");
  }
  let cart, options = null;
  let shop = {};
  try {
    cart = await currentCart();
    if (cart?.items?.length) {
      // The country list is a convenience: without it the shopper types the code.
      shop = await call("GET", "/store").catch(() => ({}));
      ({ cart, options } = await delivery(cart).catch(() => ({ cart, options: null })));
    }
  } catch (e) {
    return failed(root, e, () => location.reload());
  }
  const countries = shop.countries ?? [];
  const draw = () => {
    ready(root);
    showCount(cart);
    if (!cart?.items?.length) {
      const last = saved.get(ORDER);
      root.replaceChildren(
        h(
          "div",
          { class: "state" },
          h("h2", { text: "Your cart is empty" }),
          h("p", { class: "muted", text: "Find something you like and add it here." }),
          h("div", { class: "row" }, h("a", { class: "button", href: "/", text: "Start shopping" }), last?.number ? h("a", { class: "button quiet", href: "/order", text: `Follow order ${last.number}` }) : null),
        ),
      );
      return;
    }
    // Whatever a button does, the cart (and the delivery methods, when the call worked them out) come back and the page is drawn again.
    const act = async (fn, said) => {
      root.querySelectorAll("button, input, select").forEach((b) => (b.disabled = true));
      root.setAttribute("aria-busy", "true");
      try {
        const got = await fn();
        if (got?.items) cart = got;
        else if (got?.cart) ({ cart, options } = got);
        // A change to the lines or a code can change what delivery costs: ask again.
        if (got?.items && cart.shipping_address?.country) ({ cart, options } = await delivery(cart).catch(() => ({ cart, options })));
        draw();
        if (said) announce(said);
      } catch (e) {
        draw();
        flash(root, e.message);
      }
    };
    const lines = cart.items.map((l) => {
      const warn = !l.available ? "No longer sold: remove it to check out" : !l.in_stock ? "Not enough in stock" : null;
      return h(
        "li",
        { class: "line" },
        picture(l.image, 64),
        h("div", { class: "info" }, l.item ? h("a", { href: `/p/${encodeURIComponent(l.item)}`, text: l.title ?? l.item }) : h("span", { text: l.title ?? "" }), h("span", { class: "muted", text: money(l.price, cart.currency) }), l.added_by_promotion ? h("span", { class: "muted", text: "Added by a promotion" }) : null, warn && h("span", { class: "error", text: warn })),
        h("strong", { class: "amount", text: money(l.amount, cart.currency) }),
        h(
          "div",
          { class: "qty" },
          stepper(l, (q) => act(() => call("PATCH", `/cart/items/${encodeURIComponent(l.id)}`, { qty: q }), Number(q) === 0 ? `Removed ${l.title ?? "item"}` : `Quantity of ${l.title ?? "item"} is ${q}`)),
          h("button", { class: "quiet", type: "button", text: "Remove", "aria-label": `Remove ${l.title ?? "item"}`, onclick: () => act(() => call("DELETE", `/cart/items/${encodeURIComponent(l.id)}`), `Removed ${l.title ?? "item"}`) }),
        ),
      );
    });
    root.replaceChildren(
      h("ul", { class: "lines" }, lines),
      codesPanel(cart, act),
      placePanel(cart, countries, act),
      deliveryPanel(cart, options, act),
      totals(cart, countries),
      h("p", { class: "more" }, h("a", { class: "button", href: "/checkout", text: "Checkout" })),
      h("p", { class: "more" }, h("a", { href: "/", text: "Continue shopping" })),
    );
  };
  draw();
  if (problem) flash(root, problem);
}

// ───────────────────────── checkout ─────────────────────────

function field(label, name, value, attrs = {}) {
  return h("label", {}, label, h("input", { name, value: value ?? "", ...attrs }));
}

function addressFields(prefix, a, countries) {
  const country =
    countries.length > 0
      ? h("label", {}, "Country", h("select", { name: `${prefix}.country`, required: true, autocomplete: "country" }, h("option", { value: "", text: "Choose…" }), countries.map((c) => h("option", { value: c.code, selected: a?.country === c.code, text: c.name ?? c.code }))))
      : field("Country (two letters, such as IN)", `${prefix}.country`, a?.country, { required: true, maxlength: "2", autocomplete: "country" });
  return h(
    "div",
    { class: "fields" },
    field("Address", `${prefix}.line1`, a?.line1, { required: true, autocomplete: "address-line1" }),
    field("Address, line 2", `${prefix}.line2`, a?.line2, { autocomplete: "address-line2" }),
    field("City", `${prefix}.city`, a?.city, { required: true, autocomplete: "address-level2" }),
    field("State or region", `${prefix}.state`, a?.state, { autocomplete: "address-level1" }),
    field("Postal code", `${prefix}.postal_code`, a?.postal_code, { autocomplete: "postal-code", inputmode: "text" }),
    country,
  );
}

function readAddress(form, prefix) {
  const part = (p) => form.elements[`${prefix}.${p}`]?.value.trim() || null;
  return { line1: part("line1"), line2: part("line2"), city: part("city"), state: part("state"), postal_code: part("postal_code"), country: part("country")?.toUpperCase() ?? null };
}

/** The form control an API field name belongs to: `shipping_address.city` is `ship.city`. */
function controlFor(form, apiField) {
  if (!apiField) return null;
  const [head, part] = String(apiField).split(".");
  if (head === "gift_card" && !part) return form.elements["gift_card.code"] ?? null;
  const name = part ? `${{ shipping_address: "ship", billing_address: "bill" }[head] ?? head}.${part}` : head;
  return form.elements[name] ?? null;
}

/** Puts an error next to the control it is about, and moves focus there; false when it is about no control. */
function fieldError(form, e) {
  form.querySelectorAll(".field-error").forEach((n) => n.remove());
  form.querySelectorAll("[aria-invalid]").forEach((n) => n.removeAttribute("aria-invalid"));
  const input = controlFor(form, e.field);
  if (!input || !e.detail) return false;
  const note = h("span", { class: "field-error", id: `err-${input.name.replace(/\W/g, "-")}`, text: e.detail });
  input.setAttribute("aria-invalid", "true");
  input.setAttribute("aria-describedby", note.id);
  input.closest("label")?.append(note);
  input.focus();
  input.scrollIntoView({ block: "center" });
  return true;
}

const clear = (form) => fieldError(form, { field: null });

/** What the order is, folded away at the top of the checkout so a phone does not scroll past it. */
function summary(cart, countries = []) {
  const n = Number(cart.item_count ?? 0);
  return h(
    "details",
    { class: "summary" },
    h("summary", { text: `${n} ${n === 1 ? "item" : "items"} · ${money(cart.totals?.rounded_total ?? cart.totals?.grand_total, cart.currency)}` }),
    h("div", {}, h("ul", { class: "lines" }, cart.items.map((l) => h("li", { class: "line" }, picture(l.image, 64), h("div", { class: "info" }, h("span", { text: l.title ?? l.item ?? "" }), h("span", { class: "muted", text: `${l.qty} × ${money(l.price, cart.currency)}` })), h("strong", { class: "amount", text: money(l.amount, cart.currency) })))), totals(cart, countries)),
  );
}

function stepsBar(step) {
  return h("ol", { class: "steps", "aria-label": "Checkout steps" }, h("li", { class: step > 1 ? "done" : "", "aria-current": step === 1 ? "step" : null, text: "Your details" }), h("li", { "aria-current": step === 2 ? "step" : null, text: "Delivery and payment" }));
}

async function checkoutView(root) {
  let cart, shop;
  let countries = [];
  try {
    [cart, shop] = await Promise.all([currentCart(), call("GET", "/store")]);
    countries = shop.countries ?? [];
  } catch (e) {
    return failed(root, e, () => location.reload());
  }
  showCount(cart);
  if (!cart?.items?.length) {
    location.replace("/cart");
    return;
  }
  ready(root);
  if (!shop.allow_guest_checkout) {
    root.replaceChildren(h("div", { class: "state" }, h("h2", { text: "This store takes orders from accounts only" }), h("p", { class: "muted", text: "Contact the store to order." })));
    return;
  }
  const same = h("input", { type: "checkbox", name: "same", checked: cart.billing_same_as_shipping !== false });
  // Hidden, its fields are off too, so the browser does not ask for them.
  const billing = h("fieldset", { class: "bare", hidden: same.checked, disabled: same.checked }, h("h3", { text: "Billing address" }), addressFields("bill", cart.billing_address, []));
  same.addEventListener("change", () => (billing.hidden = billing.disabled = same.checked));
  const details = h(
    "form",
    { class: "panel", novalidate: true },
    h("h2", { text: "Your details" }),
    h(
      "div",
      { class: "fields" },
      field("Email", "email", cart.email, { type: "email", required: true, autocomplete: "email", inputmode: "email" }),
      field("Name", "name", cart.name, { required: true, autocomplete: "name" }),
      field("Phone", "phone", cart.phone, { type: "tel", autocomplete: "tel", inputmode: "tel" }),
    ),
    h("h3", { text: "Delivery address" }),
    addressFields("ship", cart.shipping_address, countries),
    h("label", { class: "choice" }, same, h("span", { text: "Bill to the same address" })),
    billing,
    h("label", {}, "Notes for the store", h("textarea", { name: "notes", rows: "2", maxlength: "2000" }, cart.notes ?? "")),
    h("label", { class: "choice" }, h("input", { type: "checkbox", name: "reminder_consent" }), h("span", {}, "Email me a reminder if I leave something in my cart", h("span", { class: "muted" }, " (optional: used for nothing else, and you can say no)"))),
    h("p", { class: "muted place-hint", id: "place-hint", text: "The country, region and postal code decide your tax and delivery. The total is worked out for them as soon as you give them." }),
    h("p", {}, h("button", { type: "submit", text: "Continue to delivery and payment" })),
  );
  const pay = h("div");
  const head = h("div", {}, stepsBar(1), summary(cart, countries));
  root.replaceChildren(head, details, pay);

  // Where it goes is taken as it is given, so the total at the top is the total that will be charged.
  const where = ["ship.country", "ship.state", "ship.postal_code"].map((n) => details.elements[n]);
  const worked = h("span", { class: "visually-hidden", role: "status" });
  details.querySelector("#place-hint").after(worked);
  let placeTimer = 0;
  const workOut = async () => {
    const [country] = where;
    if (!country?.value.trim()) return;
    try {
      cart = await call("PATCH", "/cart/details", { shipping_address: { ...readAddress(details, "ship"), country: country.value.trim().toUpperCase() } });
      head.replaceChildren(stepsBar(1), summary(cart, countries));
      worked.textContent = `Total worked out for ${placeText(cart, countries)}`;
    } catch (e) {
      fieldError(details, { field: e.field?.startsWith("shipping_address.") ? e.field : "shipping_address.country", detail: e.detail ?? e.message });
    }
  };
  for (const el of where) el?.addEventListener("change", () => (clearTimeout(placeTimer), (placeTimer = setTimeout(workOut, 150))));

  details.addEventListener("submit", async (ev) => {
    ev.preventDefault();
    const b = details.querySelector("button[type=submit]");
    // Our own checks first, so each says what is missing next to the control (the browser's bubbles say it in its own words).
    clear(details);
    for (const el of details.querySelectorAll("input[required], select[required]")) {
      if (el.disabled || el.closest("[hidden]")) continue;
      if (!el.value.trim() || (el.type === "email" && !/^\S+@\S+\.\S+$/.test(el.value.trim()))) {
        fieldError(details, { field: el.name.replace(/^ship\./, "shipping_address.").replace(/^bill\./, "billing_address."), detail: el.type === "email" && el.value.trim() ? "Enter an email address, such as you@example.com" : "Fill this in" });
        return;
      }
    }
    b.disabled = true;
    b.setAttribute("aria-busy", "true");
    flash(root, null);
    try {
      const f = details.elements;
      cart = await call("PATCH", "/cart/details", {
        email: f.email.value.trim(),
        name: f.name.value.trim(),
        phone: f.phone.value.trim() || null,
        notes: f.notes.value.trim() || null,
        reminder_consent: !!f.reminder_consent.checked,
        shipping_address: readAddress(details, "ship"),
        billing_same_as_shipping: same.checked,
        billing_address: same.checked ? null : readAddress(details, "bill"),
      });
      head.replaceChildren(stepsBar(2), summary(cart, countries));
      await payment();
    } catch (e) {
      if (!fieldError(details, e)) flash(root, e.message);
    } finally {
      b.disabled = false;
      b.removeAttribute("aria-busy");
    }
  });

  async function payment() {
    // Delivery methods for the place given (cart's country is set by now), the first chosen when none is.
    let options;
    ({ cart, options } = await delivery(cart));
    options ??= [];
    const methods = shop.payment_methods ?? [];
    const summaryBox = h("div");
    const place = h("button", { type: "submit", disabled: !methods.length && !shop.gift_cards });
    const refreshLabel = () => (place.textContent = `Place order · ${money(cart.totals?.rounded_total ?? cart.totals?.grand_total, cart.currency)}`);
    const redraw = () => {
      summaryBox.replaceChildren(totals(cart, countries));
      head.replaceChildren(stepsBar(2), summary(cart, countries));
      refreshLabel();
    };
    // Whatever changes the cart here (a delivery method, a code) draws the totals again; a refusal is said as it is.
    const act = async (fn, said) => {
      flash(root, null);
      try {
        const got = await fn();
        if (got?.items) cart = got;
        redraw();
        drawCodes();
        if (said) announce(said);
      } catch (e) {
        flash(root, e.message);
      }
    };
    const choose = (id, label) => act(() => call("PUT", "/cart/shipping", { shipping_method: id }), `Delivery: ${label ?? "chosen"}`);
    const codes = h("div");
    const drawCodes = () => codes.replaceChildren(codesPanel(cart, act));
    drawCodes();
    redraw();
    const deliveryBox = options.length
      ? h(
          "fieldset",
          { class: "panel" },
          h("legend", { text: `Delivery to ${placeText(cart, countries) || "your address"}` }),
          options.map((o) =>
            h(
              "label",
              { class: "choice" },
              h("input", { type: "radio", name: "ship", value: o.id, checked: o.id === cart.shipping?.id, onchange: (ev) => choose(ev.target.value, o.label) }),
              h("span", {}, h("strong", { text: o.label ?? "Delivery" }), ` · ${Number(o.amount) === 0 ? "Free" : money(o.amount, cart.currency)}`, o.description ? h("div", { class: "muted", text: o.description }) : null),
            ),
          ),
        )
      : (cart.missing ?? []).includes("shipping_method")
        ? h("div", { class: "panel" }, h("p", { class: "notice error", role: "status", text: "We cannot deliver to that place. Go back, change the country, region or postal code, or contact the store." }), h("p", {}, h("button", { type: "button", class: "quiet", text: "Change where it goes", onclick: () => (details.scrollIntoView(), details.elements["ship.country"]?.focus()) })))
        : null;
    // A gift card or store credit: its balance is the card's to say, so this says what will be done with it.
    const cardBox = shop.gift_cards
      ? h(
          "fieldset",
          { class: "panel card-pay" },
          h("legend", { text: "Gift card or store credit" }),
          h("p", { class: "muted", id: "card-hint", text: "Have a code? We take what the card holds, up to the total, and the rest is paid by the method you choose below." }),
          h("div", { class: "fields" }, field("Code", "gift_card.code", "", { autocomplete: "off", maxlength: "64", "aria-describedby": "card-hint" }), field("PIN, if it has one", "gift_card.pin", "", { autocomplete: "off", maxlength: "64", inputmode: "numeric" })),
          h(
            "details",
            { class: "explain" },
            h("summary", { text: "What if the card holds less than the total?" }),
            h("p", { class: "muted", text: "The part it holds is held on the card for this order, and nothing is charged until the rest is paid. Once you pay the rest, the held part is taken from the card. If the rest is not paid in time, the hold is let go, the card is untouched and the order is cancelled." }),
            h("p", { class: "muted", text: "If the card holds more than the total, only the total is taken and the balance stays on the card. Your order page says how much of the card was used." }),
          ),
        )
      : null;
    const cardCode = () => cardBox?.querySelector('[name="gift_card.code"]');
    // With a code given, the card may pay all of it: no other method is needed.
    const cardOnly = shop.gift_cards ? h("label", { class: "choice", hidden: true }, h("input", { type: "radio", name: "method", value: "" }), h("span", {}, h("strong", { text: "The card pays all of it" }), h("div", { class: "muted", text: "If it holds less than the total, the order is not placed and you choose how to pay the rest." }))) : null;
    cardCode()?.addEventListener("input", () => {
      const has = !!cardCode().value.trim();
      cardOnly.hidden = !has;
      if (!has && cardOnly.querySelector("input").checked) form.querySelector('input[name=method]:not([value=""])')?.click();
    });
    refreshLabel();
    const form = h(
      "form",
      { class: "panel" },
      h("h2", { text: "Payment" }),
      cardBox,
      methods.length
        ? [cardOnly, ...methods.map((m, i) => h("label", { class: "choice" }, h("input", { type: "radio", name: "method", value: m.gateway, checked: i === 0, required: true }), h("span", {}, h("strong", { text: m.label ?? m.gateway }), m.instructions ? h("div", { class: "muted", text: m.instructions }) : null)))]
        : shop.gift_cards
          ? [cardOnly, h("p", { class: "muted", text: "This store takes no other payment online yet: the card has to cover the whole order." })]
          : h("p", { class: "error", text: "This store takes no payments online yet. Contact the store to order." }),
      shop.terms_url ? h("label", { class: "choice" }, h("input", { type: "checkbox", name: "terms", required: true }), h("span", {}, "I accept the ", h("a", { href: shop.terms_url, target: "_blank", rel: "noopener", text: "terms" }))) : null,
      summaryBox,
      h("p", {}, place),
    );
    form.addEventListener("submit", async (ev) => {
      ev.preventDefault();
      // A second press while the first is being sent sends nothing.
      if (place.disabled) return;
      clear(form);
      place.disabled = true;
      place.setAttribute("aria-busy", "true");
      place.textContent = "Placing your order…";
      flash(root, null);
      // One key per attempt at this cart: sending it again (a retry, a double click) places one order.
      const keyName = `ferrum.checkout:${saved.get(CART)}`;
      const key = sessionStorage.getItem(keyName) ?? crypto.randomUUID();
      sessionStorage.setItem(keyName, key);
      const code = cardCode()?.value.trim();
      const chosen = form.elements.method?.value || null;
      try {
        const done = await call(
          "POST",
          "/checkout",
          {
            payment_method: code && !chosen ? null : chosen ?? (methods[0]?.gateway ?? null),
            gift_card: code ? { code, pin: form.elements["gift_card.pin"].value.trim() || undefined } : undefined,
            accept_terms: !!form.elements.terms?.checked || !shop.terms_url,
            expected_total: cart.totals?.rounded_total,
            return_url: RETURN,
          },
          { "idempotency-key": key },
        );
        saved.set(ORDER, { number: done.order?.number, email: done.order?.email ?? details.elements.email.value.trim(), token: done.order_token });
        saved.set(CART, null);
        sessionStorage.removeItem(keyName);
        await sendOn(root, done, methods.find((m) => m.gateway === chosen)?.online !== false);
      } catch (e) {
        // Prices may have changed since the page was drawn: show the new total.
        if (e.field === "expected_total") cart = (await currentCart().catch(() => null)) ?? cart;
        redraw();
        if (!fieldError(form, e)) flash(root, e.message);
        place.removeAttribute("aria-busy");
        place.disabled = false;
        refreshLabel();
      }
    });
    pay.replaceChildren(...[deliveryBox, codes, form].filter(Boolean));
    (deliveryBox ?? form).querySelector("input")?.focus();
  }
}

/** After the order is placed: to the gateway's page when there is one, waiting a little for the gateway to make it. */
async function sendOn(root, done, online) {
  const request = done.payment_request;
  const go = (url) => location.assign(typeof url === "string" && url.startsWith("https://") ? url : "/order");
  if (!online || !request || request.status !== "Initiated" || request.payment_url || !request.id) return go(request?.payment_url);
  // Online payment: the gateway writes its page for the request a moment after the order, so ask for it.
  const stop = h("a", { href: "/order", text: "Go to my order instead" });
  root.replaceChildren(h("div", { class: "state", role: "status" }, h("h2", { text: "Order placed" }), h("p", { class: "muted", text: "Taking you to the payment page…" }), h("p", {}, stop)));
  for (let i = 0; i < 12; i++) {
    await new Promise((r) => setTimeout(r, 1500));
    try {
      const now = await call("GET", `/payment-requests/${encodeURIComponent(request.id)}`, null, { "x-order-token": done.order_token });
      if (typeof now.payment_url === "string" && now.payment_url.startsWith("https://")) return go(now.payment_url);
      if (now.status !== "Initiated") break;
    } catch {
      break;
    }
  }
  go(null);
}

// ───────────────────────── the order ─────────────────────────

/** The link that opens this order for anyone who has it: the token rides in the fragment, which no server sees. */
const linkTo = (o) => `${location.origin}/order#n=${encodeURIComponent(o.number)}&t=${encodeURIComponent(o.token)}`;

/** The order asked for: from the address's fragment (a tracking link), else this browser's memory of the last order. */
function wanted() {
  const hash = new URLSearchParams(location.hash.slice(1));
  if (hash.get("n") && hash.get("t")) {
    const link = { number: hash.get("n"), token: hash.get("t") };
    const known = saved.get(ORDER);
    // The same order we placed here: keep the email, which unlocks the full view.
    return known?.number === link.number && known?.token === link.token ? known : link;
  }
  return saved.get(ORDER);
}

/** What an order is called by someone waiting for it. */
function orderWords(o) {
  if (o.status === "Cancelled") return "Cancelled";
  if (o.payment_status === "Refunded") return "Refunded";
  // Shipped, never "Delivered": nothing tells the store a parcel arrived, and an order shipped and
  // invoiced at once is Completed the moment it leaves.
  if (o.shipments?.length) return "Shipped";
  if (["Paid", "Not Required"].includes(o.payment_status) || o.payment_method === "Cash on Delivery") return "Being prepared";
  return "Waiting for payment";
}

function track(o) {
  return h(
    "ol",
    { class: "track-steps", "aria-label": "Where your order is" },
    (o.steps ?? []).map((s, i, all) => h("li", { class: s.done ? "done" : "", "aria-current": !s.done && all.slice(0, i).every((p) => p.done) ? "step" : null, text: s.label })),
  );
}

function shipments(o) {
  return (o.shipments ?? []).map((s) =>
    h(
      "div",
      { class: "shipment" },
      h("p", {}, h("strong", { text: "Shipped" }), s.date ? ` on ${new Date(`${s.date}T00:00:00`).toLocaleDateString(undefined, { day: "numeric", month: "long" })}` : ""),
      s.carrier || s.tracking_number ? h("p", { class: "muted", text: [s.carrier, s.tracking_number].filter(Boolean).join(" · ") }) : null,
      typeof s.tracking_url === "string" && s.tracking_url.startsWith("https://") ? h("p", {}, h("a", { class: "button quiet", href: s.tracking_url, target: "_blank", rel: "noopener noreferrer", text: "Track your parcel" })) : null,
    ),
  );
}

function refunds(o) {
  return (o.refunds ?? []).map((r) => {
    const amount = money(r.amount, r.currency ?? o.currency);
    // What an edit took off an order that was already paid goes back as an Adjustment.
    const why = r.kind === "Adjustment" ? " because your order was changed" : r.kind === "Cancellation" ? " for the order you cancelled" : "";
    return h("div", { class: "refund" }, h("p", {}, h("strong", { text: r.status === "Refunded" ? `${amount} refunded${why}` : `${amount} on its way back to you${why}` })), r.date ? h("p", { class: "muted", text: new Date(`${r.date}T00:00:00`).toLocaleDateString(undefined, { day: "numeric", month: "long" }) }) : null);
  });
}

/** What the order's payments came to, sorted by what they are: a gift card's part, money paid, the one still asked for, the ones that did not go through. */
function paymentsOf(o) {
  const all = o.payments ?? [];
  const other = all.filter((p) => p.gateway !== "Gift Card");
  const total = Number(o.totals?.rounded_total ?? o.totals?.grand_total ?? 0);
  const paid = other.filter((p) => p.status === "Paid");
  const asked = other.find((p) => p.status === "Initiated");
  const card = all.filter((p) => p.gateway === "Gift Card" && p.status === "Paid").reduce((n, p) => n + Number(p.amount), 0);
  // A card that covered only part holds that part, which no payment records: what the request asks for is the rest.
  const held = !card && asked && !paid.length ? Math.max(0, total - Number(asked.amount)) : 0;
  const failed = other.filter((p) => ["Failed", "Expired", "Cancelled"].includes(p.status));
  return { all, other, total, paid, asked, card, held, failed };
}

/** What a gift card did for the order, in words: its part paid, or its part held until the rest is paid. */
function cardNote(o) {
  const { card, held, asked } = paymentsOf(o);
  if (card) return h("p", { class: "notice", role: "status", text: `A gift card or store credit paid ${money(card, o.currency)} of this order.${asked ? ` The other ${money(asked.amount, asked.currency ?? o.currency)} is still to pay.` : ""}` });
  if (held > 0)
    return h(
      "p",
      { class: "notice", role: "status" },
      `Your gift card or store credit is holding ${money(held, o.currency)} for this order. Nothing has been taken from it yet: pay the remaining ${money(asked.amount, asked.currency ?? o.currency)} and the held part is taken. If the rest is not paid in time, the hold is let go, your card is untouched and the order is cancelled.`,
    );
  return null;
}

/** Your order was changed after you paid: what you have paid, what it comes to now, and the difference, asked for or sent back. */
function difference(o) {
  const { paid, asked, card, total } = paymentsOf(o);
  if (!paid.length || !asked) return null;
  const paidSum = paid.reduce((n, p) => n + Number(p.amount), 0) + card;
  return h("p", { class: "notice", role: "status", text: `Your order was changed after you paid. You have paid ${money(paidSum, o.currency)}; it now comes to ${money(total, o.currency)}, so ${money(asked.amount, asked.currency ?? o.currency)} more is due.` });
}

/** A payment that did not go through, and the way to try again: the same order, a method of your choice, no new cart. */
function retryPanel(o, link, reload) {
  const { failed, asked, paid, card, total, held } = paymentsOf(o);
  const unpaid = !asked && !paid.length && !card && !held && failed.length && !["Cancelled", "Paid", "Not Required", "Refunded", "Partly Refunded"].includes(o.payment_status ?? "") && o.status !== "Cancelled";
  if (!unpaid) return null;
  const box = h("section", { class: "panel retry", "aria-labelledby": "retry-title" }, h("h2", { id: "retry-title", text: failed[0].status === "Failed" ? "Your payment did not go through" : "Your payment was not made in time" }), h("p", { class: "muted", text: `Nothing was taken. Your order is kept for you: choose how to pay ${money(total, o.currency)} and try again.` }), skeleton());
  box.setAttribute("aria-busy", "true");
  call("GET", "/store").then(
    (shop) => {
      box.removeAttribute("aria-busy");
      const methods = shop.payment_methods ?? [];
      const email = o.email ? null : field("The email you ordered with", "email", "", { type: "email", required: true, autocomplete: "email" });
      const go = h("button", { type: "submit", text: `Try again · ${money(total, o.currency)}`, disabled: !methods.length });
      const form = h("form", { novalidate: true }, methods.map((m, i) => h("label", { class: "choice" }, h("input", { type: "radio", name: "method", value: m.gateway, checked: m.gateway === failed[0].gateway || (i === 0 && !methods.some((x) => x.gateway === failed[0].gateway)) }), h("span", {}, h("strong", { text: m.label ?? m.gateway }), m.instructions ? h("div", { class: "muted", text: m.instructions }) : null))), email, h("p", {}, go));
      form.addEventListener("submit", async (ev) => {
        ev.preventDefault();
        clear(form);
        const addr = o.email ?? form.elements.email?.value.trim();
        if (!addr) return void fieldError(form, { field: "email", detail: "Enter the email you ordered with" });
        go.disabled = true;
        go.setAttribute("aria-busy", "true");
        flash(box, null);
        try {
          const made = await call("POST", "/orders/pay", { number: link.number, email: addr, token: link.token, payment_method: form.elements.method.value, return_url: RETURN });
          saved.set(ORDER, { number: link.number, email: addr, token: link.token });
          const url = made.payment_request?.payment_url;
          if (typeof url === "string" && url.startsWith("https://")) return void location.assign(url);
          announce("A new payment was asked for");
          reload();
        } catch (e) {
          go.disabled = false;
          go.removeAttribute("aria-busy");
          if (!fieldError(form, e)) flash(box, e.message);
        }
      });
      box.querySelector(".skeleton").replaceWith(form);
      if (!methods.length) form.prepend(h("p", { class: "error", text: "This store takes no payments online yet. Contact the store." }));
    },
    (e) => (box.removeAttribute("aria-busy"), box.querySelector(".skeleton").replaceWith(h("p", { class: "error", role: "alert", text: e.message }))),
  );
  return box;
}

/** The paste-a-link form for someone with no link at hand. */
function findOrder(root, message) {
  const input = h("input", { name: "link", placeholder: "https://…/order#n=…", "aria-label": "The link from your order email", autocomplete: "off" });
  const form = h(
    "form",
    { class: "coupon", onsubmit: (ev) => (ev.preventDefault(), open(input.value)) },
    input,
    h("button", { type: "submit", text: "Find my order" }),
  );
  const open = (text) => {
    const at = String(text).indexOf("#");
    const p = new URLSearchParams(at >= 0 ? String(text).slice(at + 1) : "");
    if (!p.get("n") || !p.get("t")) {
      input.setAttribute("aria-invalid", "true");
      input.focus();
      return announce("That is not an order link. Copy the whole link from your email.");
    }
    saved.set(ORDER, { number: p.get("n"), token: p.get("t") });
    location.hash = `n=${encodeURIComponent(p.get("n"))}&t=${encodeURIComponent(p.get("t"))}`;
    location.reload();
  };
  ready(root);
  root.replaceChildren(h("div", { class: "state" }, h("h2", { text: "Find your order" }), h("p", { class: "muted", text: message }), form, h("p", {}, h("a", { href: "/", text: "Back to the store" }))));
}

async function orderView(root) {
  const o = wanted();
  if (!o?.number || !o?.token) return findOrder(root, "Open the link in your order email, or paste it here.");
  let order;
  try {
    // With the email we placed it with, the full view; with the link alone, the thinner one.
    order = (o.email ? await call("POST", "/orders/lookup", { number: o.number, email: o.email, token: o.token }) : await call("POST", "/orders/track", { number: o.number, token: o.token })).order;
  } catch (e) {
    if (e.status === 404) return findOrder(root, "We could not find an order with that link. Check you copied all of it, or paste it here.");
    return failed(root, e, () => orderView(root));
  }
  saved.set(ORDER, { number: o.number, email: o.email ?? null, token: o.token });
  const draw = (order) => {
    ready(root);
    const due = (order.payments ?? []).find((p) => p.status === "Initiated" && p.gateway !== "Gift Card");
    const more = difference(order);
    const where = order.shipping_address ? `Delivering to: ${order.shipping_address.replaceAll("\n", ", ")}` : order.ship_to ? `Delivering to ${order.ship_to}` : null;
    const lines = (order.items ?? []).map((l) => h("li", { class: "line" }, h("span", { class: "img none", "aria-hidden": "true" }), h("div", { class: "info" }, l.item ? h("a", { href: `/p/${encodeURIComponent(l.item)}`, text: l.title ?? l.item }) : h("span", { text: l.title ?? "" }), h("span", { class: "muted", text: `${l.qty} × ${money(l.price, order.currency)}` })), h("strong", { class: "amount", text: money(l.amount, order.currency) })));
    const share = h(
      "div",
      { class: "share" },
      h("p", { class: "muted", text: "Anyone with this link can follow this order. Keep it to yourself." }),
      h("button", { type: "button", class: "quiet", text: "Copy the link", onclick: async (ev) => {
        try {
          await navigator.clipboard.writeText(linkTo(o));
          ev.target.textContent = "Copied";
          announce("Link copied");
        } catch {
          announce("Your browser would not copy the link. Select the address and copy it by hand.");
        }
      } }),
    );
    root.replaceChildren(
      ...[
        h("p", {}, h("span", { class: "status", text: orderWords(order) }), ` Order ${order.number}`),
        order.status === "Cancelled" ? null : track(order),
        cardNote(order),
        more,
        retryPanel(order, o, () => orderView(root)),
        due?.payment_url?.startsWith("https://") ? h("p", {}, h("a", { class: "button", href: due.payment_url, text: `${more ? "Pay the difference" : "Pay"} · ${money(due.amount, due.currency ?? order.currency)}` })) : due && !isOffline(due) ? h("p", { class: "notice", role: "status", text: "Your payment link is being made. This page updates by itself." }) : null,
        due?.instructions ? h("p", { class: "muted", text: due.instructions }) : null,
        ...shipments(order),
        ...refunds(order),
        h("ul", { class: "lines" }, lines),
        totals({ ...order, shipping: order.shipping?.method ? { label: order.shipping.method } : null }, [], false),
        where ? h("p", { class: "muted", text: where }) : null,
        share,
      ].filter(Boolean),
    );
    return due;
  };
  const isOffline = (p) => !p.payment_url && ["Manual", "Cash on Delivery"].includes(p.gateway);
  // While a payment link is being made, ask again; and when the shopper comes back to this tab, look again.
  const again = async () => {
    try {
      const r = o.email ? await call("POST", "/orders/lookup", { number: o.number, email: o.email, token: o.token }) : await call("POST", "/orders/track", { number: o.number, token: o.token });
      return draw(r.order);
    } catch {
      return undefined;
    }
  };
  let due = draw(order);
  for (let i = 0; i < 10 && due && !due.payment_url && !isOffline(due); i++) {
    await new Promise((r) => setTimeout(r, 2000));
    due = await again();
  }
  document.addEventListener("visibilitychange", () => document.visibilityState === "visible" && void again());
}

// ───────────────────────── start ─────────────────────────

for (const form of document.querySelectorAll("form.add")) {
  form.addEventListener("submit", async (ev) => {
    ev.preventDefault();
    const button = form.querySelector("button");
    const out = form.querySelector(".added");
    button.disabled = true;
    button.setAttribute("aria-busy", "true");
    out.classList.remove("error");
    out.textContent = "";
    try {
      showCount(await addToCart(form.dataset.item, form.elements.qty?.value || "1"));
      out.replaceChildren("Added to your cart. ", h("a", { href: "/cart", text: "View cart" }));
    } catch (e) {
      out.classList.add("error");
      out.textContent = e.message;
    } finally {
      button.disabled = false;
      button.removeAttribute("aria-busy");
    }
  });
}

// ───────────────────────── the product page ─────────────────────────

/** The price a variant shows, as the page shows it (with tax when the store says prices include it). */
function priceOf(v) {
  const p = v?.price;
  if (!p) return null;
  const shown = page.tax === "true" && p.price_with_tax ? p.price_with_tax : p.price;
  return shown == null ? null : money(shown, p.currency);
}

/** Whether a variant has exactly these choices. */
const has = (v, choice) => Object.entries(choice).every(([name, value]) => (v.options ?? []).some((o) => o.option === name && o.value === value));

/** What a choice among an option's values comes to, given what is chosen on the others: not offered (no such combination), sold out, or available with its price and stock. */
export function describe(variants, current, name, value, showStock) {
  const wanted = { ...current, [name]: value };
  const v = variants.find((x) => has(x, wanted));
  if (!v) {
    const others = Object.entries(current).filter(([n]) => n !== name).map(([, val]) => val).join(" / ");
    return { state: "unavailable", words: others ? `Not offered with ${others}` : "Not offered", variant: null };
  }
  if (v.purchasable === false) return { state: "unavailable", words: "Not for sale", variant: v };
  if (!v.in_stock) return { state: "soldout", words: "Sold out", price: priceOf(v), variant: v };
  const words = showStock && v.available_qty != null ? `${v.available_qty} in stock` : "In stock";
  return { state: "available", words, price: priceOf(v), variant: v };
}

/** The product page's option picker told what the server knows of every combination: its price and stock, and which are sold out or not offered. */
async function enhancePicker(buy, slug) {
  const groups = [...buy.querySelectorAll("fieldset.option")];
  if (!groups.length) return null;
  let item;
  try {
    item = await call("GET", `/items/${encodeURIComponent(slug)}`);
  } catch {
    return null; // the plain links still work
  }
  const variants = item.variants ?? [];
  const showStock = variants.some((v) => v.available_qty != null);
  const nameOf = (g) => g.querySelector("legend").firstChild.textContent.replace(/:\s*$/, "").trim();
  const current = Object.fromEntries(groups.map((g) => [nameOf(g), g.querySelector(".pick.on")?.textContent.trim()]).filter(([, v]) => v));
  for (const g of groups) {
    const name = nameOf(g);
    for (const a of g.querySelectorAll(".pick")) {
      const d = describe(variants, current, name, a.querySelector(".pick-value")?.textContent ?? a.textContent.trim(), showStock);
      a.classList.toggle("off", d.state !== "available");
      a.classList.toggle("na", d.state === "unavailable");
      if (!a.querySelector(".pick-value")) a.replaceChildren(h("span", { class: "pick-value", text: a.textContent.trim() }));
      a.querySelectorAll(".pick-extra, .visually-hidden").forEach((n) => n.remove());
      if (d.price && g === groups[0] && variants.some((x) => priceOf(x) !== priceOf(variants[0]))) a.append(h("span", { class: "pick-extra", text: d.price }));
      a.append(h("span", { class: "visually-hidden", text: `, ${d.words}${d.price ? `, ${d.price}` : ""}` }));
      a.title = `${d.words}${d.price ? ` · ${d.price}` : ""}`;
    }
  }
  // The combination in front of the shopper: its price and its stock, in words under the picker.
  const now = variants.find((v) => has(v, current));
  if (now) {
    const stock = !now.purchasable ? "Not for sale" : now.in_stock ? (showStock && now.available_qty != null ? `${now.available_qty} in stock` : "In stock") : "Sold out";
    buy.querySelector(".picks")?.closest("fieldset")?.after(h("p", { class: "combo muted", role: "status", text: `${Object.values(current).join(" / ")}: ${[priceOf(now), stock].filter(Boolean).join(" · ")}` }));
  }
  return now?.slug ?? null;
}

/** Out of stock: leave an email to be told when it is back, once. For a variant, the one shown. */
function stockAlert(buy, slug, variant) {
  if (buy.querySelector("form.add") || !buy.querySelector("p.out")) return;
  const want = variant ?? new URLSearchParams(location.search).get("v") ?? slug;
  const email = h("input", { type: "email", name: "email", required: true, autocomplete: "email", inputmode: "email", "aria-describedby": "alert-hint", placeholder: "you@example.com" });
  const out = h("p", { class: "added", role: "status" });
  const button = h("button", { class: "button", type: "submit", text: "Tell me when it is back" });
  const form = h(
    "form",
    { class: "alert-box", novalidate: true, "aria-labelledby": "alert-title" },
    h("h2", { id: "alert-title", text: "Back in stock" }),
    h("p", { class: "muted", id: "alert-hint", text: "Leave your email and we will write once, when it is back. We use it for nothing else." }),
    h("label", {}, "Email", email),
    button,
    out,
  );
  form.addEventListener("submit", async (ev) => {
    ev.preventDefault();
    out.classList.remove("error");
    out.textContent = "";
    clear(form);
    if (!/^\S+@\S+\.\S+$/.test(email.value.trim())) return void fieldError(form, { field: "email", detail: "Enter an email address, such as you@example.com" });
    button.disabled = true;
    button.setAttribute("aria-busy", "true");
    try {
      await call("POST", `/items/${encodeURIComponent(want)}/stock-alert`, { email: email.value.trim() });
      out.textContent = "Thank you. We will write to you once, when it is back.";
      email.value = "";
    } catch (e) {
      out.classList.add("error");
      out.textContent = e.message;
    } finally {
      button.disabled = false;
      button.removeAttribute("aria-busy");
    }
  });
  buy.append(form);
}

let productReady;
const buyBox = document.querySelector(".product .buy");
if (buyBox) {
  const slug = decodeURIComponent(location.pathname.split("/")[2] ?? "");
  productReady = enhancePicker(buyBox, slug).then((variant) => stockAlert(buyBox, slug, variant));
}

// A thumbnail shows its picture large.
const main = document.querySelector(".gallery > img");
for (const thumb of document.querySelectorAll(".thumbs img")) {
  thumb.tabIndex = 0;
  thumb.setAttribute("role", "button");
  const show = () => {
    if (!main) return;
    const [src, alt] = [main.getAttribute("src"), main.getAttribute("alt")];
    main.setAttribute("src", thumb.getAttribute("src"));
    main.setAttribute("alt", thumb.getAttribute("alt") ?? "");
    thumb.setAttribute("src", src);
    thumb.setAttribute("alt", alt ?? "");
  };
  thumb.addEventListener("click", show);
  thumb.addEventListener("keydown", (ev) => (ev.key === "Enter" || ev.key === " ") && (ev.preventDefault(), show()));
}

const view = document.getElementById("view");
const views = { cart: cartView, checkout: checkoutView, order: orderView };
let viewReady;
if (view && views[view.dataset.view]) {
  viewReady = views[view.dataset.view](view).catch((e) => failed(view, e, () => location.reload()));
} else {
  viewReady = currentCart().then(showCount, () => {});
}
// Back to a page from the browser's memory: the cart may have changed since.
window.addEventListener("pageshow", (ev) => ev.persisted && currentCart().then(showCount, () => {}));

/** Settled once the page has drawn what it draws (for a test, which has no other way to know). */
export const started = Promise.all([viewReady, productReady]);
