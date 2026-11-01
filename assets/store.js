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

const LABELS = { email: "Email", name: "Name", phone: "Phone", shipping_address: "Delivery address", billing_address: "Billing address", qty: "Quantity", code: "Code" };

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

function totals(cart) {
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
  let cart;
  try {
    cart = await currentCart();
  } catch (e) {
    return failed(root, e, () => location.reload());
  }
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
    const act = async (fn, said) => {
      root.querySelectorAll("button, input").forEach((b) => (b.disabled = true));
      root.setAttribute("aria-busy", "true");
      try {
        cart = await fn();
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
        h("div", { class: "info" }, l.item ? h("a", { href: `/p/${encodeURIComponent(l.item)}`, text: l.title ?? l.item }) : h("span", { text: l.title ?? "" }), h("span", { class: "muted", text: money(l.price, cart.currency) }), warn && h("span", { class: "error", text: warn })),
        h("strong", { class: "amount", text: money(l.amount, cart.currency) }),
        h(
          "div",
          { class: "qty" },
          stepper(l, (q) => act(() => call("PATCH", `/cart/items/${encodeURIComponent(l.id)}`, { qty: q }), Number(q) === 0 ? `Removed ${l.title ?? "item"}` : `Quantity of ${l.title ?? "item"} is ${q}`)),
          h("button", { class: "quiet", type: "button", text: "Remove", "aria-label": `Remove ${l.title ?? "item"}`, onclick: () => act(() => call("DELETE", `/cart/items/${encodeURIComponent(l.id)}`), `Removed ${l.title ?? "item"}`) }),
        ),
      );
    });
    const code = h("input", { name: "code", placeholder: "Discount code", "aria-label": "Discount code", maxlength: "60", autocomplete: "off" });
    const coupon = cart.coupon
      ? h("p", {}, `Code ${cart.coupon.code} applied. `, h("button", { class: "link", type: "button", text: "Remove code", onclick: () => act(() => call("DELETE", "/cart/coupon"), "Discount code removed") }))
      : h(
          "form",
          { class: "coupon", onsubmit: (ev) => (ev.preventDefault(), code.value.trim() && act(() => call("PUT", "/cart/coupon", { code: code.value.trim() }), "Discount code applied")) },
          code,
          h("button", { class: "quiet", type: "submit", text: "Apply" }),
        );
    root.replaceChildren(h("ul", { class: "lines" }, lines), coupon, totals(cart), h("p", { class: "more" }, h("a", { class: "button", href: "/checkout", text: "Checkout" })), h("p", { class: "more" }, h("a", { href: "/", text: "Continue shopping" })));
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
function summary(cart) {
  const n = Number(cart.item_count ?? 0);
  return h(
    "details",
    { class: "summary" },
    h("summary", { text: `${n} ${n === 1 ? "item" : "items"} · ${money(cart.totals?.rounded_total ?? cart.totals?.grand_total, cart.currency)}` }),
    h("div", {}, h("ul", { class: "lines" }, cart.items.map((l) => h("li", { class: "line" }, picture(l.image, 64), h("div", { class: "info" }, h("span", { text: l.title ?? l.item ?? "" }), h("span", { class: "muted", text: `${l.qty} × ${money(l.price, cart.currency)}` })), h("strong", { class: "amount", text: money(l.amount, cart.currency) })))), totals(cart)),
  );
}

function stepsBar(step) {
  return h("ol", { class: "steps", "aria-label": "Checkout steps" }, h("li", { class: step > 1 ? "done" : "", "aria-current": step === 1 ? "step" : null, text: "Your details" }), h("li", { "aria-current": step === 2 ? "step" : null, text: "Delivery and payment" }));
}

async function checkoutView(root) {
  let cart, shop;
  try {
    [cart, shop] = await Promise.all([currentCart(), call("GET", "/store")]);
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
  const countries = shop.countries ?? [];
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
    h("p", {}, h("button", { type: "submit", text: "Continue to delivery and payment" })),
  );
  const pay = h("div");
  const head = h("div", {}, stepsBar(1), summary(cart));
  root.replaceChildren(head, details, pay);

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
        shipping_address: readAddress(details, "ship"),
        billing_same_as_shipping: same.checked,
        billing_address: same.checked ? null : readAddress(details, "bill"),
      });
      head.replaceChildren(stepsBar(2), summary(cart));
      await payment();
    } catch (e) {
      if (!fieldError(details, e)) flash(root, e.message);
    } finally {
      b.disabled = false;
      b.removeAttribute("aria-busy");
    }
  });

  async function payment() {
    const { options = [], selected = null } = await call("GET", "/cart/shipping-options");
    const methods = shop.payment_methods ?? [];
    const summaryBox = h("div");
    const place = h("button", { type: "submit", disabled: !methods.length });
    const refreshLabel = () => (place.textContent = `Place order · ${money(cart.totals?.rounded_total ?? cart.totals?.grand_total, cart.currency)}`);
    const choose = async (id) => {
      cart = await call("PUT", "/cart/shipping", { shipping_method: id });
      summaryBox.replaceChildren(totals(cart));
      head.replaceChildren(stepsBar(2), summary(cart));
      refreshLabel();
    };
    if (options.length && !options.some((o) => o.id === selected)) await choose(options[0].id);
    summaryBox.replaceChildren(totals(cart));
    const delivery = options.length
      ? h(
          "fieldset",
          { class: "panel" },
          h("legend", { text: "Delivery" }),
          options.map((o) =>
            h(
              "label",
              { class: "choice" },
              h("input", { type: "radio", name: "ship", value: o.id, checked: o.id === (cart.shipping?.id ?? selected ?? options[0].id), onchange: (ev) => choose(ev.target.value).catch((e) => flash(root, e.message)) }),
              h("span", {}, h("strong", { text: o.label ?? "Delivery" }), ` · ${money(o.amount, cart.currency)}`, o.description ? h("div", { class: "muted", text: o.description }) : null),
            ),
          ),
        )
      : null;
    refreshLabel();
    const form = h(
      "form",
      { class: "panel" },
      h("h2", { text: "Payment" }),
      methods.length
        ? methods.map((m, i) =>
            h("label", { class: "choice" }, h("input", { type: "radio", name: "method", value: m.gateway, checked: i === 0, required: true }), h("span", {}, h("strong", { text: m.label ?? m.gateway }), m.instructions ? h("div", { class: "muted", text: m.instructions }) : null)),
          )
        : h("p", { class: "error", text: "This store takes no payments online yet. Contact the store to order." }),
      shop.terms_url ? h("label", { class: "choice" }, h("input", { type: "checkbox", name: "terms", required: true }), h("span", {}, "I accept the ", h("a", { href: shop.terms_url, target: "_blank", rel: "noopener", text: "terms" }))) : null,
      summaryBox,
      h("p", {}, place),
    );
    form.addEventListener("submit", async (ev) => {
      ev.preventDefault();
      place.disabled = true;
      place.setAttribute("aria-busy", "true");
      place.textContent = "Placing your order…";
      flash(root, null);
      // One key per attempt at this cart: sending it again (a retry, a double click) places one order.
      const keyName = `ferrum.checkout:${saved.get(CART)}`;
      const key = sessionStorage.getItem(keyName) ?? crypto.randomUUID();
      sessionStorage.setItem(keyName, key);
      try {
        const done = await call(
          "POST",
          "/checkout",
          { payment_method: form.elements.method?.value ?? (methods[0]?.gateway ?? null), accept_terms: !!form.elements.terms?.checked || !shop.terms_url, expected_total: cart.totals?.rounded_total, return_url: RETURN },
          { "idempotency-key": key },
        );
        saved.set(ORDER, { number: done.order?.number, email: done.order?.email ?? details.elements.email.value.trim(), token: done.order_token });
        saved.set(CART, null);
        sessionStorage.removeItem(keyName);
        const chosen = form.elements.method?.value ?? methods[0]?.gateway;
        await sendOn(root, done, methods.find((m) => m.gateway === chosen)?.online !== false);
      } catch (e) {
        // Prices may have changed since the page was drawn: show the new total.
        if (e.field === "expected_total") cart = (await currentCart().catch(() => null)) ?? cart;
        summaryBox.replaceChildren(totals(cart));
        flash(root, e.message);
        place.removeAttribute("aria-busy");
        place.disabled = false;
        refreshLabel();
      }
    });
    pay.replaceChildren(...[delivery, form].filter(Boolean));
    form.querySelector("input")?.focus();
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
  if (o.shipments?.length) return o.status === "Completed" || o.status === "Closed" ? "Delivered" : "Shipped";
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
      s.tracking_url ? h("p", {}, h("a", { class: "button quiet", href: s.tracking_url, target: "_blank", rel: "noopener noreferrer", text: "Track your parcel" })) : null,
    ),
  );
}

function refunds(o) {
  return (o.refunds ?? []).map((r) =>
    h("div", { class: "refund" }, h("p", {}, h("strong", { text: r.status === "Refunded" ? `${money(r.amount, r.currency ?? o.currency)} refunded` : `${money(r.amount, r.currency ?? o.currency)} on its way back to you` })), r.date ? h("p", { class: "muted", text: new Date(`${r.date}T00:00:00`).toLocaleDateString(undefined, { day: "numeric", month: "long" }) }) : null),
  );
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
    const due = (order.payments ?? []).find((p) => p.status === "Initiated");
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
        due?.payment_url?.startsWith("https://") ? h("p", {}, h("a", { class: "button", href: due.payment_url, text: `Pay ${money(due.amount, due.currency ?? order.currency)}` })) : due && !isOffline(due) ? h("p", { class: "notice", role: "status", text: "Your payment link is being made. This page updates by itself." }) : null,
        due?.instructions ? h("p", { class: "muted", text: due.instructions }) : null,
        ...shipments(order),
        ...refunds(order),
        h("ul", { class: "lines" }, lines),
        totals({ ...order, shipping: order.shipping?.method ? { label: order.shipping.method } : null }),
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
if (view && views[view.dataset.view]) {
  views[view.dataset.view](view).catch((e) => failed(view, e, () => location.reload()));
} else {
  currentCart().then(showCount, () => {});
}
// Back to a page from the browser's memory: the cart may have changed since.
window.addEventListener("pageshow", (ev) => ev.persisted && currentCart().then(showCount, () => {}));
