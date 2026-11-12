// The storefront's script (src/pages.rs): the cart's count in the header, "Add to cart", the product
// page's pictures, and the cart, checkout and order pages, over the storefront API the page names in
// <body data-api>. Everything it shows from the API is set as text, never as markup. Every view has
// its loading, empty and failed states, and says what it does to a screen reader.

const page = document.body.dataset;
const API = page.api;

// The page's words: its language's, with English under every one that language has not, written into the page by the
// server (src/pages.rs) so no word of the shop lives in this script. `t("key", {name: value})` fills {name}.
const LANG = document.documentElement.lang || "en";
const WORDS = (() => {
  try {
    return JSON.parse(document.getElementById("store-words")?.textContent ?? "{}");
  } catch {
    return {};
  }
})();
const t = (key, vars = {}) => (WORDS[key] ?? key).replace(/\{(\w+)\}/g, (m, name) => (vars[name] == null ? m : String(vars[name])));
const plural = new Intl.PluralRules(LANG);
/** A word that depends on a count: `key.one`, `key.few`, … as the language counts, else `key.other`. */
const tn = (key, n, vars = {}) => t(WORDS[`${key}.${plural.select(n)}`] != null ? `${key}.${plural.select(n)}` : `${key}.other`, { n, ...vars });
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

const LABELS = ["email", "name", "phone", "shipping_address", "billing_address", "qty", "code", "gift_card", "country"];

class ApiError extends Error {
  constructor(status, data) {
    const f = data?.fields?.[0];
    const head = String(f?.field ?? "").split(".")[0];
    const where = f?.field ? t("error.field", { field: LABELS.includes(head) ? t(`label.${head}`) : f.field, message: f.message ?? "" }) : "";
    super(f?.message ? where : (data?.message ?? t("error.generic", { status })));
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
    throw new ApiError(0, { message: t("error.unreachable") });
  }
  const data = await r.json().catch(() => null);
  if (!r.ok) throw new ApiError(r.status, data);
  return data;
}

const nf = (() => {
  const options = { minimumFractionDigits: PRECISION, maximumFractionDigits: PRECISION };
  try {
    return new Intl.NumberFormat(LANG, options);
  } catch {
    return new Intl.NumberFormat(undefined, options);
  }
})();
const money = (v, code = CURRENCY) => t("money", { code, amount: nf.format(Number(v ?? 0)) });

/** A country's name in the shopper's own language, from its two-letter code; the code when the browser does not know it. */
function countryName(code, countries = []) {
  if (!code) return "";
  const listed = countries.find((c) => c.code === code)?.name;
  if (listed) return listed;
  try {
    return new Intl.DisplayNames(LANG, { type: "region" }).of(code) ?? code;
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
      h("h2", { text: error.status === 404 ? t("error.notfound") : t("error.title") }),
      h("p", { class: "muted", text: error.message }),
      h("div", { class: "row" }, retry ? h("button", { type: "button", text: t("action.retry"), onclick: retry }) : null, h("a", { class: "button quiet", href: "/", text: t("action.store") })),
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
  badge.setAttribute("aria-label", n ? t("cart.count_aria", { n }) : "");
}

function totals(cart, countries = [], estimate = true) {
  const sums = cart.totals ?? {};
  const rows = [[t("totals.subtotal"), sums.subtotal]];
  if (Number(sums.discount)) rows.push([t("totals.discount"), -Number(sums.discount)]);
  if (cart.shipping) rows.push([t("totals.delivery", { label: cart.shipping.label ?? t("totals.shipping") }), sums.shipping]);
  for (const tax of sums.taxes ?? []) rows.push([tax.description ?? t("totals.tax"), tax.amount]);
  if (Number(sums.rounding_adjustment)) rows.push([t("totals.rounding"), sums.rounding_adjustment]);
  const box = h("dl", { class: "totals" });
  for (const [label, amount] of rows) box.append(h("dt", { text: label }), h("dd", { text: money(amount, cart.currency) }));
  box.append(h("dt", { class: "grand", text: t("totals.total") }), h("dd", { class: "grand", text: money(sums.rounded_total ?? sums.grand_total, cart.currency) }));
  if (cart.prices_include_tax) box.append(h("dt", { class: "muted", text: t("totals.tax_included") }), h("dd"));
  // What the total is for: the place its tax and delivery were worked out for, or that it does not have them yet.
  if (estimate) {
    const where = placeText(cart, countries);
    box.append(h("dt", { class: "muted place-note", text: where ? t("totals.place_known", { place: where }) : t("totals.place_unknown") }), h("dd"));
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
  const input = h("input", { type: "number", min: "0", step: "any", inputmode: "decimal", value: l.qty, "aria-label": t("stepper.qty", { item: l.title ?? t("item") }) });
  const set = (n) => {
    input.value = String(Math.max(0, n));
    onChange(input.value || "0");
  };
  input.addEventListener("change", () => onChange(input.value || "0"));
  return h(
    "div",
    { class: "stepper" },
    h("button", { type: "button", class: "quiet", text: "−", "aria-label": t("stepper.fewer", { item: l.title ?? t("item") }), onclick: () => set(Number(input.value || 0) - 1) }),
    input,
    h("button", { type: "button", class: "quiet", text: "+", "aria-label": t("stepper.more", { item: l.title ?? t("item") }), onclick: () => set(Number(input.value || 0) + 1) }),
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
    ? h("label", {}, t("label.country"), h("select", { name: "country", autocomplete: "country" }, h("option", { value: "", text: t("select.choose") }), countries.map((c) => h("option", { value: c.code, selected: a.country === c.code, text: c.name ?? c.code }))))
    : field(t("field.country_code"), "country", a.country, { maxlength: "2", autocomplete: "country" });
  const form = h(
    "form",
    { class: "panel place", "aria-labelledby": "place-title", novalidate: true },
    h("h2", { id: "place-title", text: t("place.title") }),
    h("p", { class: "muted", text: t("place.text") }),
    h("div", { class: "fields" }, country, field(t("field.state"), "state", a.state, { autocomplete: "address-level1" }), field(t("field.postal_code"), "postal_code", a.postal_code, { autocomplete: "postal-code" })),
    h("p", {}, h("button", { class: "quiet", type: "submit", text: a.country ? t("place.update") : t("place.show") })),
  );
  form.addEventListener("submit", (ev) => {
    ev.preventDefault();
    clear(form);
    const f = form.elements;
    if (!f.country.value.trim()) return void fieldError(form, { field: "country", detail: t("place.choose") });
    const next = { ...a, country: f.country.value.trim().toUpperCase(), state: f.state.value.trim() || null, postal_code: f.postal_code.value.trim() || null };
    act(async () => {
      const done = await delivery(await call("PATCH", "/cart/details", { shipping_address: next }));
      return done;
    }, t("place.done"));
  });
  return form;
}

/** The delivery methods that reach the place, with what each costs (free when it is), or why there are none. */
function deliveryPanel(cart, options, act) {
  if (!options) return null;
  if (!options.length) {
    return (cart.missing ?? []).includes("shipping_method")
      ? h("p", { class: "notice error", role: "status", text: t("delivery.none") })
      : null;
  }
  return h(
    "fieldset",
    { class: "panel" },
    h("legend", { text: t("delivery.title") }),
    options.map((o) =>
      h(
        "label",
        { class: "choice" },
        h("input", { type: "radio", name: "ship", value: o.id, checked: o.id === cart.shipping?.id, onchange: (ev) => act(() => call("PUT", "/cart/shipping", { shipping_method: ev.target.value }).then((c) => ({ cart: c, options })), t("delivery.chosen", { label: o.label ?? t("delivery.chosen_default") })) }),
        h("span", {}, h("strong", { text: o.label ?? t("delivery.title") }), ` · ${Number(o.amount) === 0 ? t("delivery.free") : money(o.amount, cart.currency)}`, o.description ? h("div", { class: "muted", text: o.description }) : null),
      ),
    ),
  );
}

/** Promotion codes (several) and the store's own coupon (one): what each took off, and which do not apply. */
function codesPanel(cart, act) {
  const codes = cart.codes ?? [];
  const row = (code, words, remove) =>
    h("li", {}, h("strong", { text: code }), ` ${words} `, h("button", { class: "link", type: "button", text: t("codes.remove"), "aria-label": t("codes.remove_aria", { code }), onclick: () => act(remove, t("codes.removed", { code })) }));
  const input = h("input", { name: "code", placeholder: t("codes.placeholder"), "aria-label": t("codes.placeholder"), maxlength: "60", autocomplete: "off" });
  const form = h(
    "form",
    { class: "coupon", onsubmit: (ev) => (ev.preventDefault(), input.value.trim() && act(() => call("PUT", "/cart/coupon", { code: input.value.trim() }), t("codes.applied_toast"))) },
    input,
    h("button", { class: "quiet", type: "submit", text: t("codes.apply") }),
  );
  const took = (cart.promotions ?? []).filter((p) => Number(p.amount));
  return h(
    "div",
    { class: "codes" },
    h("ul", { class: "applied" }, cart.coupon ? row(cart.coupon.code, t("codes.applied"), () => call("DELETE", "/cart/coupon")) : null, codes.map((c) => row(c.code, c.applied ? t("codes.applied") : t("codes.not_applicable"), () => call("DELETE", `/cart/codes/${encodeURIComponent(c.code)}`)))),
    took.length ? h("ul", { class: "took" }, took.map((p) => h("li", { class: "muted", text: t("codes.took", { title: p.title, amount: money(p.amount, cart.currency) }) }))) : null,
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
          h("h2", { text: t("cart.empty") }),
          h("p", { class: "muted", text: t("cart.empty_text") }),
          h("div", { class: "row" }, h("a", { class: "button", href: "/", text: t("cart.start") }), last?.number ? h("a", { class: "button quiet", href: "/order", text: t("cart.follow", { number: last.number }) }) : null),
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
      const warn = !l.available ? t("line.gone") : !l.in_stock ? t("line.short") : null;
      return h(
        "li",
        { class: "line" },
        picture(l.image, 64),
        h("div", { class: "info" }, l.item ? h("a", { href: `/p/${encodeURIComponent(l.item)}`, text: l.title ?? l.item }) : h("span", { text: l.title ?? "" }), h("span", { class: "muted", text: money(l.price, cart.currency) }), l.added_by_promotion ? h("span", { class: "muted", text: t("line.promoted") }) : null, warn && h("span", { class: "error", text: warn })),
        h("strong", { class: "amount", text: money(l.amount, cart.currency) }),
        h(
          "div",
          { class: "qty" },
          stepper(l, (q) => act(() => call("PATCH", `/cart/items/${encodeURIComponent(l.id)}`, { qty: q }), Number(q) === 0 ? t("line.removed", { item: l.title ?? t("item") }) : t("line.qty_is", { item: l.title ?? t("item"), qty: q }))),
          h("button", { class: "quiet", type: "button", text: t("codes.remove"), "aria-label": t("line.remove_aria", { item: l.title ?? t("item") }), onclick: () => act(() => call("DELETE", `/cart/items/${encodeURIComponent(l.id)}`), t("line.removed", { item: l.title ?? t("item") })) }),
        ),
      );
    });
    root.replaceChildren(
      h("ul", { class: "lines" }, lines),
      codesPanel(cart, act),
      placePanel(cart, countries, act),
      deliveryPanel(cart, options, act),
      totals(cart, countries),
      h("p", { class: "more" }, h("a", { class: "button", href: "/checkout", text: t("cart.checkout") })),
      h("p", { class: "more" }, h("a", { href: "/", text: t("cart.continue") })),
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
      ? h("label", {}, t("label.country"), h("select", { name: `${prefix}.country`, required: true, autocomplete: "country" }, h("option", { value: "", text: t("select.choose") }), countries.map((c) => h("option", { value: c.code, selected: a?.country === c.code, text: c.name ?? c.code }))))
      : field(t("field.country_code"), `${prefix}.country`, a?.country, { required: true, maxlength: "2", autocomplete: "country" });
  return h(
    "div",
    { class: "fields" },
    field(t("field.address"), `${prefix}.line1`, a?.line1, { required: true, autocomplete: "address-line1" }),
    field(t("field.address2"), `${prefix}.line2`, a?.line2, { autocomplete: "address-line2" }),
    field(t("field.city"), `${prefix}.city`, a?.city, { required: true, autocomplete: "address-level2" }),
    field(t("field.state"), `${prefix}.state`, a?.state, { autocomplete: "address-level1" }),
    field(t("field.postal_code"), `${prefix}.postal_code`, a?.postal_code, { autocomplete: "postal-code", inputmode: "text" }),
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
    h("summary", { text: `${tn("cart.items", n)} · ${money(cart.totals?.rounded_total ?? cart.totals?.grand_total, cart.currency)}` }),
    h("div", {}, h("ul", { class: "lines" }, cart.items.map((l) => h("li", { class: "line" }, picture(l.image, 64), h("div", { class: "info" }, h("span", { text: l.title ?? l.item ?? "" }), h("span", { class: "muted", text: t("line.times", { qty: l.qty, price: money(l.price, cart.currency) }) })), h("strong", { class: "amount", text: money(l.amount, cart.currency) })))), totals(cart, countries)),
  );
}

function stepsBar(step) {
  return h("ol", { class: "steps", "aria-label": t("steps.label") }, h("li", { class: step > 1 ? "done" : "", "aria-current": step === 1 ? "step" : null, text: t("steps.details") }), h("li", { "aria-current": step === 2 ? "step" : null, text: t("steps.payment") }));
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
    root.replaceChildren(h("div", { class: "state" }, h("h2", { text: t("checkout.accounts_only") }), h("p", { class: "muted", text: t("checkout.contact") })));
    return;
  }
  const same = h("input", { type: "checkbox", name: "same", checked: cart.billing_same_as_shipping !== false });
  // Hidden, its fields are off too, so the browser does not ask for them.
  const billing = h("fieldset", { class: "bare", hidden: same.checked, disabled: same.checked }, h("h3", { text: t("label.billing_address") }), addressFields("bill", cart.billing_address, []));
  same.addEventListener("change", () => (billing.hidden = billing.disabled = same.checked));
  const details = h(
    "form",
    { class: "panel", novalidate: true },
    h("h2", { text: t("steps.details") }),
    h(
      "div",
      { class: "fields" },
      field(t("label.email"), "email", cart.email, { type: "email", required: true, autocomplete: "email", inputmode: "email" }),
      field(t("label.name"), "name", cart.name, { required: true, autocomplete: "name" }),
      field(t("label.phone"), "phone", cart.phone, { type: "tel", autocomplete: "tel", inputmode: "tel" }),
    ),
    h("h3", { text: t("label.shipping_address") }),
    addressFields("ship", cart.shipping_address, countries),
    h("label", { class: "choice" }, same, h("span", { text: t("checkout.same") })),
    billing,
    h("label", {}, t("checkout.notes"), h("textarea", { name: "notes", rows: "2", maxlength: "2000" }, cart.notes ?? "")),
    h("label", { class: "choice" }, h("input", { type: "checkbox", name: "reminder_consent" }), h("span", {}, t("checkout.reminder"), h("span", { class: "muted" }, ` ${t("checkout.reminder_note")}`))),
    h("p", { class: "muted place-hint", id: "place-hint", text: t("checkout.place_hint") }),
    h("p", {}, h("button", { type: "submit", text: t("checkout.continue") })),
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
      worked.textContent = t("checkout.worked", { place: placeText(cart, countries) });
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
        fieldError(details, { field: el.name.replace(/^ship\./, "shipping_address.").replace(/^bill\./, "billing_address."), detail: el.type === "email" && el.value.trim() ? t("error.email") : t("error.required") });
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
    const refreshLabel = () => (place.textContent = t("checkout.place", { total: money(cart.totals?.rounded_total ?? cart.totals?.grand_total, cart.currency) }));
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
    const choose = (id, label) => act(() => call("PUT", "/cart/shipping", { shipping_method: id }), t("delivery.chosen", { label: label ?? t("delivery.chosen_default") }));
    const codes = h("div");
    const drawCodes = () => codes.replaceChildren(codesPanel(cart, act));
    drawCodes();
    redraw();
    const deliveryBox = options.length
      ? h(
          "fieldset",
          { class: "panel" },
          h("legend", { text: t("delivery.to", { place: placeText(cart, countries) || t("delivery.your_address") }) }),
          options.map((o) =>
            h(
              "label",
              { class: "choice" },
              h("input", { type: "radio", name: "ship", value: o.id, checked: o.id === cart.shipping?.id, onchange: (ev) => choose(ev.target.value, o.label) }),
              h("span", {}, h("strong", { text: o.label ?? t("delivery.title") }), ` · ${Number(o.amount) === 0 ? t("delivery.free") : money(o.amount, cart.currency)}`, o.description ? h("div", { class: "muted", text: o.description }) : null),
            ),
          ),
        )
      : (cart.missing ?? []).includes("shipping_method")
        ? h("div", { class: "panel" }, h("p", { class: "notice error", role: "status", text: t("delivery.none_back") }), h("p", {}, h("button", { type: "button", class: "quiet", text: t("delivery.change"), onclick: () => (details.scrollIntoView(), details.elements["ship.country"]?.focus()) })))
        : null;
    // A gift card or store credit: its balance is the card's to say, so this says what will be done with it.
    const cardBox = shop.gift_cards
      ? h(
          "fieldset",
          { class: "panel card-pay" },
          h("legend", { text: t("card.title") }),
          h("p", { class: "muted", id: "card-hint", text: t("card.hint") }),
          h("div", { class: "fields" }, field(t("label.code"), "gift_card.code", "", { autocomplete: "off", maxlength: "64", "aria-describedby": "card-hint" }), field(t("card.pin"), "gift_card.pin", "", { autocomplete: "off", maxlength: "64", inputmode: "numeric" })),
          h(
            "details",
            { class: "explain" },
            h("summary", { text: t("card.less") }),
            h("p", { class: "muted", text: t("card.less_text") }),
            h("p", { class: "muted", text: t("card.more_text") }),
          ),
        )
      : null;
    const cardCode = () => cardBox?.querySelector('[name="gift_card.code"]');
    // With a code given, the card may pay all of it: no other method is needed.
    const cardOnly = shop.gift_cards ? h("label", { class: "choice", hidden: true }, h("input", { type: "radio", name: "method", value: "" }), h("span", {}, h("strong", { text: t("card.all") }), h("div", { class: "muted", text: t("card.all_text") }))) : null;
    cardCode()?.addEventListener("input", () => {
      const has = !!cardCode().value.trim();
      cardOnly.hidden = !has;
      if (!has && cardOnly.querySelector("input").checked) form.querySelector('input[name=method]:not([value=""])')?.click();
    });
    refreshLabel();
    const form = h(
      "form",
      { class: "panel" },
      h("h2", { text: t("payment.title") }),
      cardBox,
      methods.length
        ? [cardOnly, ...methods.map((m, i) => h("label", { class: "choice" }, h("input", { type: "radio", name: "method", value: m.gateway, checked: i === 0, required: true }), h("span", {}, h("strong", { text: m.label ?? m.gateway }), m.instructions ? h("div", { class: "muted", text: m.instructions }) : null)))]
        : shop.gift_cards
          ? [cardOnly, h("p", { class: "muted", text: t("payment.card_only") })]
          : h("p", { class: "error", text: t("payment.none_order") }),
      shop.terms_url ? h("label", { class: "choice" }, h("input", { type: "checkbox", name: "terms", required: true }), h("span", {}, `${t("terms.accept")} `, h("a", { href: shop.terms_url, target: "_blank", rel: "noopener", text: t("terms.link") }))) : null,
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
      place.textContent = t("checkout.placing");
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
  const go = (url) => location.assign(typeof url === "string" && url.startsWith("https://") ? url : withLang("/order"));
  if (!online || !request || request.status !== "Initiated" || request.payment_url || !request.id) return go(request?.payment_url);
  // Online payment: the gateway writes its page for the request a moment after the order, so ask for it.
  const stop = h("a", { href: withLang("/order"), text: t("sendon.instead") });
  root.replaceChildren(h("div", { class: "state", role: "status" }, h("h2", { text: t("sendon.placed") }), done.order ? cardNote(done.order) : null, h("p", { class: "muted", text: t("sendon.taking") }), h("p", {}, stop)));
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
  if (o.status === "Cancelled") return t("order.cancelled");
  if (o.payment_status === "Refunded") return t("order.refunded");
  // Shipped, never "Delivered": nothing tells the store a parcel arrived, and an order shipped and
  // invoiced at once is Completed the moment it leaves.
  if (o.shipments?.length) return t("order.shipped");
  if (["Paid", "Not Required"].includes(o.payment_status) || o.payment_method === "Cash on Delivery") return t("order.preparing");
  return t("order.waiting");
}

function track(o) {
  return h(
    "ol",
    { class: "track-steps", "aria-label": t("order.where") },
    (o.steps ?? []).map((s, i, all) => h("li", { class: s.done ? "done" : "", "aria-current": !s.done && all.slice(0, i).every((p) => p.done) ? "step" : null, text: s.label })),
  );
}

function shipments(o) {
  return (o.shipments ?? []).map((s) =>
    h(
      "div",
      { class: "shipment" },
      h("p", {}, h("strong", { text: t("order.shipped") }), s.date ? ` ${t("shipment.on", { date: new Date(`${s.date}T00:00:00`).toLocaleDateString(LANG, { day: "numeric", month: "long" }) })}` : ""),
      s.carrier || s.tracking_number ? h("p", { class: "muted", text: [s.carrier, s.tracking_number].filter(Boolean).join(" · ") }) : null,
      typeof s.tracking_url === "string" && s.tracking_url.startsWith("https://") ? h("p", {}, h("a", { class: "button quiet", href: s.tracking_url, target: "_blank", rel: "noopener noreferrer", text: t("shipment.track") })) : null,
    ),
  );
}

function refunds(o) {
  return (o.refunds ?? []).map((r) => {
    const amount = money(r.amount, r.currency ?? o.currency);
    // What an edit took off an order that was already paid goes back as an Adjustment.
    const why = r.kind === "Adjustment" ? t("refund.changed") : r.kind === "Cancellation" ? t("refund.cancelled") : "";
    return h("div", { class: "refund" }, h("p", {}, h("strong", { text: t(r.status === "Refunded" ? "refund.done" : "refund.coming", { amount, why: why ? ` ${why}` : "" }) })), r.date ? h("p", { class: "muted", text: new Date(`${r.date}T00:00:00`).toLocaleDateString(LANG, { day: "numeric", month: "long" }) }) : null);
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
  const { card, held, asked, failed, total } = paymentsOf(o);
  const gift = o.gift_card;
  // What the card has left, and when a hold lets go (the server says both; never the card's code).
  const left = gift?.balance_left != null ? t("note.card_left", { amount: money(gift.balance_left, o.currency) }) : "";
  const until = gift?.held_until ? t("note.card_until", { date: new Date(gift.held_until).toLocaleString(LANG, { dateStyle: "medium", timeStyle: "short" }) }) : "";
  const more = (text) => [text, until, left].filter(Boolean).join(" ");
  // The rest of the order: what is asked for now, or what is still to pay after its payment did not go through.
  const rest = asked ? money(asked.amount, asked.currency ?? o.currency) : failed.length && total - card > 0 && o.status !== "Cancelled" ? money(total - card, o.currency) : "";
  if (card) return h("p", { class: "notice", role: "status", text: more([t("note.card_paid", { amount: money(card, o.currency) }), rest ? t("note.card_rest", { amount: rest }) : ""].filter(Boolean).join(" ")) });
  if (held > 0)
    return h(
      "p",
      { class: "notice", role: "status" },
      more(t("note.card_held", { held: money(held, o.currency), rest: money(asked.amount, asked.currency ?? o.currency) })),
    );
  return null;
}

/** Your order was changed after you paid: what you have paid, what it comes to now, and the difference, asked for or sent back. */
function difference(o) {
  const { paid, asked, card, total } = paymentsOf(o);
  if (!paid.length || !asked) return null;
  const paidSum = paid.reduce((n, p) => n + Number(p.amount), 0) + card;
  return h("p", { class: "notice", role: "status", text: t("note.changed", { paid: money(paidSum, o.currency), total: money(total, o.currency), due: money(asked.amount, asked.currency ?? o.currency) }) });
}

/** A payment that did not go through, and the way to try again: the same order, a method of your choice, no new cart. A gift card's part stays paid: what is asked again is the rest (the server works it out the same way). */
function retryPanel(o, link, reload) {
  const { failed, asked, paid, card, total, held } = paymentsOf(o);
  const rest = total - card;
  const done = ["Paid", "Not Required", "Refunded", "Partly Refunded"].includes(o.payment_status ?? "") || (o.payment_status === "Cancelled" && !card);
  const unpaid = !asked && !paid.length && !held && failed.length && rest > 0 && !done && o.status !== "Cancelled";
  if (!unpaid) return null;
  const box = h("section", { class: "panel retry", "aria-labelledby": "retry-title" }, h("h2", { id: "retry-title", text: failed[0].status === "Failed" ? t("retry.failed") : t("retry.expired") }), h("p", { class: "muted", text: card ? t("retry.text_card", { card: money(card, o.currency), total: money(rest, o.currency) }) : t("retry.text", { total: money(rest, o.currency) }) }), skeleton());
  box.setAttribute("aria-busy", "true");
  call("GET", "/store").then(
    (shop) => {
      box.removeAttribute("aria-busy");
      const methods = shop.payment_methods ?? [];
      const email = o.email ? null : field(t("retry.email"), "email", "", { type: "email", required: true, autocomplete: "email" });
      const go = h("button", { type: "submit", text: t("retry.again", { total: money(rest, o.currency) }), disabled: !methods.length });
      const form = h("form", { novalidate: true }, methods.map((m, i) => h("label", { class: "choice" }, h("input", { type: "radio", name: "method", value: m.gateway, checked: m.gateway === failed[0].gateway || (i === 0 && !methods.some((x) => x.gateway === failed[0].gateway)) }), h("span", {}, h("strong", { text: m.label ?? m.gateway }), m.instructions ? h("div", { class: "muted", text: m.instructions }) : null))), email, h("p", {}, go));
      form.addEventListener("submit", async (ev) => {
        ev.preventDefault();
        clear(form);
        const addr = o.email ?? form.elements.email?.value.trim();
        if (!addr) return void fieldError(form, { field: "email", detail: t("retry.email_error") });
        go.disabled = true;
        go.setAttribute("aria-busy", "true");
        flash(box, null);
        try {
          const made = await call("POST", "/orders/pay", { number: link.number, email: addr, token: link.token, payment_method: form.elements.method.value, return_url: RETURN });
          saved.set(ORDER, { number: link.number, email: addr, token: link.token });
          const url = made.payment_request?.payment_url;
          if (typeof url === "string" && url.startsWith("https://")) return void location.assign(url);
          announce(t("retry.asked"));
          reload();
        } catch (e) {
          go.disabled = false;
          go.removeAttribute("aria-busy");
          if (!fieldError(form, e)) flash(box, e.message);
        }
      });
      box.querySelector(".skeleton").replaceWith(form);
      if (!methods.length) form.prepend(h("p", { class: "error", text: t("payment.none") }));
    },
    (e) => (box.removeAttribute("aria-busy"), box.querySelector(".skeleton").replaceWith(h("p", { class: "error", role: "alert", text: e.message }))),
  );
  return box;
}

/** The paste-a-link form for someone with no link at hand. */
function findOrder(root, message) {
  const input = h("input", { name: "link", placeholder: "https://…/order#n=…", "aria-label": t("find.link"), autocomplete: "off" });
  const form = h(
    "form",
    { class: "coupon", onsubmit: (ev) => (ev.preventDefault(), open(input.value)) },
    input,
    h("button", { type: "submit", text: t("find.button") }),
  );
  const open = (text) => {
    const at = String(text).indexOf("#");
    const p = new URLSearchParams(at >= 0 ? String(text).slice(at + 1) : "");
    if (!p.get("n") || !p.get("t")) {
      input.setAttribute("aria-invalid", "true");
      input.focus();
      return announce(t("find.bad"));
    }
    saved.set(ORDER, { number: p.get("n"), token: p.get("t") });
    location.hash = `n=${encodeURIComponent(p.get("n"))}&t=${encodeURIComponent(p.get("t"))}`;
    location.reload();
  };
  ready(root);
  root.replaceChildren(h("div", { class: "state" }, h("h2", { text: t("find.title") }), h("p", { class: "muted", text: message }), form, h("p", {}, h("a", { href: "/", text: t("find.back") }))));
}

async function orderView(root) {
  const o = wanted();
  if (!o?.number || !o?.token) return findOrder(root, t("find.open"));
  let order;
  try {
    // With the email we placed it with, the full view; with the link alone, the thinner one.
    order = (o.email ? await call("POST", "/orders/lookup", { number: o.number, email: o.email, token: o.token }) : await call("POST", "/orders/track", { number: o.number, token: o.token })).order;
  } catch (e) {
    if (e.status === 404) return findOrder(root, t("find.missing"));
    return failed(root, e, () => orderView(root));
  }
  saved.set(ORDER, { number: o.number, email: o.email ?? null, token: o.token });
  const draw = (order) => {
    ready(root);
    const due = (order.payments ?? []).find((p) => p.status === "Initiated" && p.gateway !== "Gift Card");
    const more = difference(order);
    const where = order.shipping_address ? t("order.delivering_to", { place: order.shipping_address.replaceAll("\n", ", ") }) : order.ship_to ? t("order.delivering_to", { place: order.ship_to }) : null;
    const lines = (order.items ?? []).map((l) => h("li", { class: "line" }, h("span", { class: "img none", "aria-hidden": "true" }), h("div", { class: "info" }, l.item ? h("a", { href: `/p/${encodeURIComponent(l.item)}`, text: l.title ?? l.item }) : h("span", { text: l.title ?? "" }), h("span", { class: "muted", text: t("line.times", { qty: l.qty, price: money(l.price, order.currency) }) })), h("strong", { class: "amount", text: money(l.amount, order.currency) })));
    const share = h(
      "div",
      { class: "share" },
      h("p", { class: "muted", text: t("share.text") }),
      h("button", { type: "button", class: "quiet", text: t("share.copy"), onclick: async (ev) => {
        try {
          await navigator.clipboard.writeText(linkTo(o));
          ev.target.textContent = t("share.copied");
          announce(t("share.announce"));
        } catch {
          announce(t("share.failed"));
        }
      } }),
    );
    root.replaceChildren(
      ...[
        h("p", {}, h("span", { class: "status", text: orderWords(order) }), ` ${t("order.number", { number: order.number })}`),
        order.status === "Cancelled" ? null : track(order),
        cardNote(order),
        more,
        retryPanel(order, o, () => orderView(root)),
        due?.payment_url?.startsWith("https://") ? h("p", {}, h("a", { class: "button", href: due.payment_url, text: t(more ? "order.pay_difference" : "order.pay", { amount: money(due.amount, due.currency ?? order.currency) }) })) : due && !isOffline(due) ? h("p", { class: "notice", role: "status", text: t("order.link_coming") }) : null,
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

// ───────────────────────── the language ─────────────────────────

/** The language a visitor chose (`?lang=`) goes with them: the server reads it from the address, so each link of the store carries it. */
const CHOSEN = new URLSearchParams(location.search).get("lang");
const withLang = (href) => {
  if (!CHOSEN) return href;
  try {
    const to = new URL(href, location.origin);
    if (to.origin !== location.origin || /^\/(assets|files)\//.test(to.pathname) || to.searchParams.has("lang")) return href;
    to.searchParams.set("lang", CHOSEN);
    return to.pathname + to.search + to.hash;
  } catch {
    return href;
  }
};
if (CHOSEN) {
  const carry = (a) => a.setAttribute("href", withLang(a.getAttribute("href")));
  document.querySelectorAll("a[href^='/']").forEach(carry);
  // Links the script makes later carry it when they are followed.
  document.addEventListener("click", (ev) => { const a = ev.target.closest?.("a[href^='/']"); if (a) carry(a); }, true);
  for (const form of document.querySelectorAll("form.search")) form.append(h("input", { type: "hidden", name: "lang", value: CHOSEN }));
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
      out.replaceChildren(`${t("cart.added")} `, h("a", { href: "/cart", text: t("cart.view") }));
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
    return { state: "unavailable", words: others ? t("variant.not_with", { others }) : t("variant.not_offered"), variant: null };
  }
  if (v.purchasable === false) return { state: "unavailable", words: t("variant.not_for_sale"), variant: v };
  if (!v.in_stock) return { state: "soldout", words: t("variant.sold_out"), price: priceOf(v), variant: v };
  const words = showStock && v.available_qty != null ? t("variant.qty_in_stock", { qty: v.available_qty }) : t("variant.in_stock");
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
      a.append(h("span", { class: "visually-hidden", text: `, ${[d.words, d.price].filter(Boolean).join(", ")}` }));
      a.title = `${d.words}${d.price ? ` · ${d.price}` : ""}`;
    }
  }
  // The combination in front of the shopper: its price and its stock, in words under the picker.
  const now = variants.find((v) => has(v, current));
  if (now) {
    const stock = !now.purchasable ? t("variant.not_for_sale") : now.in_stock ? (showStock && now.available_qty != null ? t("variant.qty_in_stock", { qty: now.available_qty }) : t("variant.in_stock")) : t("variant.sold_out");
    buy.querySelector(".picks")?.closest("fieldset")?.after(h("p", { class: "combo muted", role: "status", text: `${Object.values(current).join(" / ")}: ${[priceOf(now), stock].filter(Boolean).join(" · ")}` }));
  }
  return now?.slug ?? null;
}

/** Out of stock: leave an email to be told when it is back, once. For a variant, the one shown. */
function stockAlert(buy, slug, variant) {
  if (buy.querySelector("form.add") || !buy.querySelector("p.out")) return;
  const want = variant ?? new URLSearchParams(location.search).get("v") ?? slug;
  const email = h("input", { type: "email", name: "email", required: true, autocomplete: "email", inputmode: "email", "aria-describedby": "alert-hint", placeholder: t("alert.placeholder") });
  const out = h("p", { class: "added", role: "status" });
  const button = h("button", { class: "button", type: "submit", text: t("alert.button") });
  const form = h(
    "form",
    { class: "alert-box", novalidate: true, "aria-labelledby": "alert-title" },
    h("h2", { id: "alert-title", text: t("alert.title") }),
    h("p", { class: "muted", id: "alert-hint", text: t("alert.hint") }),
    h("label", {}, t("label.email"), email),
    button,
    out,
  );
  form.addEventListener("submit", async (ev) => {
    ev.preventDefault();
    out.classList.remove("error");
    out.textContent = "";
    clear(form);
    if (!/^\S+@\S+\.\S+$/.test(email.value.trim())) return void fieldError(form, { field: "email", detail: t("error.email") });
    button.disabled = true;
    button.setAttribute("aria-busy", "true");
    try {
      await call("POST", `/items/${encodeURIComponent(want)}/stock-alert`, { email: email.value.trim() });
      out.textContent = t("alert.thanks");
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
