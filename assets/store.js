// The storefront's script (src/pages.rs): the cart's count in the header, "Add to cart", and the
// cart, checkout and order pages, over the storefront API the page names in <body data-api>.
// Everything it shows from the API is set as text, never as markup.

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

function flash(root, message) {
  root.querySelector(".error")?.remove();
  if (message) root.prepend(h("p", { class: "error", role: "alert", text: message }));
}

async function cartView(root) {
  const params = new URLSearchParams(location.search);
  if (params.get("add")) {
    // "Add to cart" before its script had loaded.
    try {
      await addToCart(params.get("add"), params.get("qty") || "1");
    } catch (e) {
      flash(root, e.message);
    }
    history.replaceState(null, "", "/cart");
  }
  let cart = await currentCart();
  const draw = () => {
    showCount(cart);
    if (!cart?.items?.length) {
      root.replaceChildren(h("p", { class: "muted", text: "Your cart is empty." }), h("p", {}, h("a", { class: "button", href: "/", text: "Start shopping" })));
      return;
    }
    const act = async (fn) => {
      root.querySelectorAll("button, input").forEach((b) => (b.disabled = true));
      try {
        cart = await fn();
        draw();
      } catch (e) {
        draw();
        flash(root, e.message);
      }
    };
    const lines = cart.items.map((l) => {
      const warn = !l.available ? "No longer sold" : !l.in_stock ? "Not enough in stock" : null;
      const qty = h("input", { type: "number", min: "0", step: "any", value: l.qty, "aria-label": `Quantity of ${l.title ?? "item"}` });
      qty.addEventListener("change", () => act(() => call("PATCH", `/cart/items/${encodeURIComponent(l.id)}`, { qty: qty.value || "0" })));
      return h(
        "li",
        { class: "line" },
        picture(l.image, 64),
        h("div", {}, l.item ? h("a", { href: `/p/${encodeURIComponent(l.item)}`, text: l.title ?? l.item }) : h("span", { text: l.title ?? "" }), h("div", { class: "muted", text: money(l.price, cart.currency) }), warn && h("div", { class: "error", text: warn })),
        h(
          "div",
          { class: "qty" },
          qty,
          h("strong", { text: money(l.amount, cart.currency) }),
          h("button", { class: "quiet", type: "button", text: "Remove", "aria-label": `Remove ${l.title ?? "item"}`, onclick: () => act(() => call("DELETE", `/cart/items/${encodeURIComponent(l.id)}`)) }),
        ),
      );
    });
    const code = h("input", { name: "code", placeholder: "Discount code", "aria-label": "Discount code", maxlength: "60" });
    const coupon = cart.coupon
      ? h("p", {}, `Code ${cart.coupon.code} applied. `, h("button", { class: "quiet", type: "button", text: "Remove code", onclick: () => act(() => call("DELETE", "/cart/coupon")) }))
      : h(
          "form",
          { class: "coupon", onsubmit: (ev) => (ev.preventDefault(), code.value.trim() && act(() => call("PUT", "/cart/coupon", { code: code.value.trim() }))) },
          code,
          h("button", { class: "quiet", type: "submit", text: "Apply" }),
        );
    root.replaceChildren(h("ul", { class: "lines" }, lines), coupon, totals(cart), h("p", { class: "more" }, h("a", { class: "button", href: "/checkout", text: "Checkout" })));
  };
  draw();
}

// ───────────────────────── checkout ─────────────────────────

function field(label, name, value, attrs = {}) {
  return h("label", {}, label, h("input", { name, value: value ?? "", ...attrs }));
}

function addressFields(prefix, a, countries) {
  const country =
    countries.length > 0
      ? h("label", {}, "Country", h("select", { name: `${prefix}.country`, required: true }, h("option", { value: "", text: "Choose…" }), countries.map((c) => h("option", { value: c.code, selected: a?.country === c.code, text: c.name ?? c.code }))))
      : field("Country (two letters, such as IN)", `${prefix}.country`, a?.country, { required: true, maxlength: "2", autocomplete: "country" });
  return h(
    "div",
    { class: "fields" },
    field("Address", `${prefix}.line1`, a?.line1, { required: true, autocomplete: "address-line1" }),
    field("Address, line 2", `${prefix}.line2`, a?.line2, { autocomplete: "address-line2" }),
    field("City", `${prefix}.city`, a?.city, { required: true, autocomplete: "address-level2" }),
    field("State or region", `${prefix}.state`, a?.state, { autocomplete: "address-level1" }),
    field("Postal code", `${prefix}.postal_code`, a?.postal_code, { autocomplete: "postal-code" }),
    country,
  );
}

function readAddress(form, prefix) {
  const part = (p) => form.elements[`${prefix}.${p}`]?.value.trim() || null;
  return { line1: part("line1"), line2: part("line2"), city: part("city"), state: part("state"), postal_code: part("postal_code"), country: part("country")?.toUpperCase() ?? null };
}

async function checkoutView(root) {
  let [cart, shop] = await Promise.all([currentCart(), call("GET", "/store")]);
  showCount(cart);
  if (!cart?.items?.length) {
    location.replace("/cart");
    return;
  }
  if (!shop.allow_guest_checkout) {
    root.replaceChildren(h("p", { class: "notice", text: "This store takes orders from its customers' accounts only. Contact the store to order." }));
    return;
  }
  const countries = shop.countries ?? [];
  const same = h("input", { type: "checkbox", name: "same", checked: cart.billing_same_as_shipping !== false });
  // Hidden, its fields are off too, so the browser does not ask for them.
  const billing = h("fieldset", { class: "bare", hidden: same.checked, disabled: same.checked }, h("h3", { text: "Billing address" }), addressFields("bill", cart.billing_address, []));
  same.addEventListener("change", () => (billing.hidden = billing.disabled = same.checked));
  const details = h(
    "form",
    { class: "panel" },
    h("h2", { text: "Your details" }),
    h(
      "div",
      { class: "fields" },
      field("Email", "email", cart.email, { type: "email", required: true, autocomplete: "email" }),
      field("Name", "name", cart.name, { required: true, autocomplete: "name" }),
      field("Phone", "phone", cart.phone, { type: "tel", autocomplete: "tel" }),
    ),
    h("h3", { text: "Delivery address" }),
    addressFields("ship", cart.shipping_address, countries),
    h("label", { class: "choice" }, same, h("span", { text: "Bill to the same address" })),
    billing,
    h("label", {}, "Notes for the store", h("textarea", { name: "notes", rows: "2", maxlength: "2000" }, cart.notes ?? "")),
    h("p", {}, h("button", { type: "submit", text: "Continue" })),
  );
  const pay = h("div");
  root.replaceChildren(details, pay);

  details.addEventListener("submit", async (ev) => {
    ev.preventDefault();
    const b = details.querySelector("button[type=submit]");
    b.disabled = true;
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
      await payment();
    } catch (e) {
      flash(root, e.message);
    } finally {
      b.disabled = false;
    }
  });

  async function payment() {
    const { options = [], selected = null } = await call("GET", "/cart/shipping-options");
    const summary = h("div");
    const choose = async (id) => {
      cart = await call("PUT", "/cart/shipping", { shipping_method: id });
      summary.replaceChildren(totals(cart));
    };
    if (options.length && !options.some((o) => o.id === selected)) await choose(options[0].id);
    summary.replaceChildren(totals(cart));
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
    const methods = shop.payment_methods ?? [];
    const form = h(
      "form",
      { class: "panel" },
      h("h2", { text: "Payment" }),
      methods.length
        ? methods.map((m, i) =>
            h("label", { class: "choice" }, h("input", { type: "radio", name: "method", value: m.gateway, checked: i === 0, required: true }), h("span", {}, h("strong", { text: m.label ?? m.gateway }), m.instructions ? h("div", { class: "muted", text: m.instructions }) : null)),
          )
        : h("p", { class: "error", text: "This store takes no payments online yet." }),
      shop.terms_url ? h("label", { class: "choice" }, h("input", { type: "checkbox", name: "terms", required: true }), h("span", {}, "I accept the ", h("a", { href: shop.terms_url, target: "_blank", rel: "noopener", text: "terms" }))) : null,
      summary,
      h("p", {}, h("button", { type: "submit", text: "Place order", disabled: !methods.length })),
    );
    form.addEventListener("submit", async (ev) => {
      ev.preventDefault();
      const b = form.querySelector("button[type=submit]");
      b.disabled = true;
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
        const url = done.payment_request?.payment_url;
        location.assign(typeof url === "string" && url.startsWith("https://") ? url : "/order");
      } catch (e) {
        // Prices may have changed since the page was drawn: show the new total.
        if (e.field === "expected_total") cart = (await currentCart()) ?? cart;
        summary.replaceChildren(totals(cart));
        flash(root, e.message);
        b.disabled = false;
      }
    });
    pay.replaceChildren(...[delivery, form].filter(Boolean));
    form.querySelector("input")?.focus();
  }
}

// ───────────────────────── the order ─────────────────────────

async function orderView(root) {
  const o = saved.get(ORDER);
  if (!o?.number || !o?.token) {
    root.replaceChildren(h("p", { class: "muted", text: "Your order's details are in the email the store sent you." }));
    return;
  }
  const { order } = await call("POST", "/orders/lookup", { number: o.number, email: o.email, token: o.token });
  const due = (order.payments ?? []).find((p) => p.status === "Initiated");
  const lines = (order.items ?? []).map((l) => h("li", { class: "line" }, h("span", { class: "img none", "aria-hidden": "true" }), h("div", {}, h("span", { text: l.title ?? l.item ?? "" }), h("div", { class: "muted", text: `${l.qty} × ${money(l.price, order.currency)}` })), h("strong", { text: money(l.amount, order.currency) })));
  const parts = [
    h("p", { class: "notice" }, h("strong", { text: `Thank you. Your order is ${order.number}.` }), ` ${order.status ?? ""}${order.payment_status ? ` · Payment: ${order.payment_status}` : ""}`),
    due?.payment_url?.startsWith("https://") ? h("p", {}, h("a", { class: "button", href: due.payment_url, text: `Pay ${money(due.amount, due.currency ?? order.currency)}` })) : null,
    due?.instructions ? h("p", { class: "muted", text: due.instructions }) : null,
    h("ul", { class: "lines" }, lines),
    totals({ ...order, shipping: order.shipping?.method ? { label: order.shipping.method } : null }),
    order.shipping_address ? h("p", { class: "muted", text: `Delivering to: ${order.shipping_address}` }) : null,
  ];
  root.replaceChildren(...parts.filter(Boolean));
}

// ───────────────────────── start ─────────────────────────

for (const form of document.querySelectorAll("form.add")) {
  form.addEventListener("submit", async (ev) => {
    ev.preventDefault();
    const button = form.querySelector("button");
    const out = form.querySelector(".added");
    button.disabled = true;
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
    }
  });
}

const view = document.getElementById("view");
const views = { cart: cartView, checkout: checkoutView, order: orderView };
if (view && views[view.dataset.view]) {
  views[view.dataset.view](view).catch((e) => {
    view.replaceChildren(h("p", { class: "error", role: "alert", text: e.message }));
  });
} else {
  currentCart().then(showCount, () => {});
}
