# Commerce — the online store

Sell online from the same items, prices, stock and books as the rest of Ferrum. Package
`commerce` (depends on `core`, `selling`, `inventory`, `accounting`); logic in
`crates/pkg-commerce`.

* **Catalog** — `ItemCategory` (a tree), `WebsiteItem` (the published face of an `Item`: slug,
  title, descriptions, images, categories, related items, SEO, ranking, weight, order limit) and
  moderated `ItemReview`s.
* **Carts** — `ShoppingCart` is a sales-shaped document. Every change re-prices it with the
  selling engine (`ferrum_domain::pricing::calculate`: price list, pricing rules, tax template,
  rounding), so cart, sales order and invoice agree to the cent. Clients choose items,
  quantities and details — never prices, totals, customers or statuses.
* **Shipping** — `ShippingRule`: a fixed charge or bands over the cart subtotal or weight,
  limited to countries. The charge is a service line (the store's *Shipping Item*), so it is
  taxed, invoiced and booked like any other line.
* **Coupons** — `CouponCode` unlocks a `PricingRule` (validity dates, total and per-customer
  limits, minimum order). Saving a coupon writes `doc.coupon_rule == '<rule id>'` into the rule's
  condition; only a cart carrying a validated coupon satisfies it, so the discount reaches
  nothing else (staff orders, other carts). The rule keeps that gate while coupons use it.
* **Checkout** — one transaction: customer, final pricing, stock re-check, a submitted
  `SalesOrder`, and a `PaymentRequest`. Idempotent per cart and client key.
* **Payments** — gateway plugins confirm through the payment request's `mark_paid` action,
  which books a `PaymentEntry` (allocated to the order's open invoices, otherwise an advance that
  is applied automatically when the order's invoice is submitted).

## Setting up a store

1. **Store Settings** (one per tenant): company, currency, selling price list, tax template
   (empty: the company's default sales template), *display prices with tax*, warehouses whose
   stock sells online (empty: all of the company's), fulfilment warehouse, *enforce stock*,
   *show stock quantity* (otherwise in/out only), guest checkout, default customer group,
   shipping item (a non-stock service item), allowed countries (ISO alpha-2; empty: any),
   payment methods, terms page, storefront URL, limits (quantity per item, items per cart),
   abandoned/expired cart thresholds, and *cancel unpaid orders* (releases stock when an online
   payment request expires). The storefront answers `404` until settings exist.
2. **Payment methods** (child rows of the settings): `gateway` is a payment gateway plugin's
   name (e.g. `stripe`), `Manual` or `Cash on Delivery`; `label` and `instructions` are shown to
   customers; `payment_account` is the bank/cash account receipts are booked to (default: the
   company bank account); `expiry_minutes` (0 = never).
3. **Website Items** for the items to sell (published), **Item Categories**, **Shipping Rules**,
   **Coupon Codes** with their **Pricing Rules**.
4. Give staff the **Website Manager** role (catalog, settings, reviews, shipping, coupons,
   payment requests; they also get notified of new web orders and payments). Sales users see
   web orders among sales orders (filter *Web Order*); orders carry *Web Payment* status,
   contact email/phone, ship-to/bill-to, shipping method, coupon and customer notes.

Images are stored as URLs. `/files/{id}` uploads need a signed-in user to download, so public
storefronts should use public (CDN) image URLs.

## Storefront API

| | Public (anyone) | Signed in (portal customers, staff) |
|---|---|---|
| Base | `/api/site/{tenant}/commerce` | `/api/app/commerce` with `Authorization: Bearer …` |

Both bases serve the same routes; the signed-in one adds the customer's own orders and invoices
and prices carts for the customer's account. Conventions:

* JSON bodies (at most 64 KB), JSON responses, `Cache-Control: no-store`.
* Decimals are **strings** (`"1149.88"`), dates `YYYY-MM-DD`, times RFC 3339 UTC.
* Cart endpoints need the cart token in **`X-Cart-Token`**; guest order endpoints the order
  token from checkout (in the body, or **`X-Order-Token`**). Keep both out of URLs.
* Errors: `{"error": code, "message": text, "fields"?: [{"field", "message", "row"?: ["items", i]}]}`
  with `422` (`validation_failed` / `invalid`), `404` (`not_found`), `409` (`conflict`),
  `403` (`forbidden`), `401` (`unauthorized`), `429` (rate limited per IP by the server), `503`.
  Field names are the API's (`shipping_address.city`, `qty`, `coupon_code`, …).
* Responses carry no costs, valuations or internal ids beyond what the UI needs (cart line
  ids, shipping method ids, payment request ids), and exact stock only when the store shows it.

### Store and catalog

**`GET /store`** → `{name, currency, currency_precision, display_prices_with_tax,
show_stock_quantity, allow_guest_checkout, terms_url, countries: [{code, name}],
payment_methods: [{gateway, label, instructions, online}], max_qty_per_line, max_cart_lines}`.

**`GET /categories`** → `{categories: [{slug, name, image, description, seo: {title,
description}, children: [...]}]}` — published categories (an unpublished one hides its branch).

**`GET /catalog`** — query: `category` (slug; includes subcategories), `q` (title, short
description or item code; ≤ 100 chars), `min_price`, `max_price` (list price in the store
currency, before tax and discounts), `in_stock=1`, `sort` (`ranking` (default: rank, then
newest), `price_asc`, `price_desc`, `newest`, `name`, `rating`), `limit` (1–48, default 24),
`cursor` (from `next_cursor`; valid for the same sort) →
`{items: [Card], next_cursor, currency}`. Only published items of enabled items with a price.

`Card` = `{slug, title, short_description, image: {url, alt} | null, price: Price,
in_stock, available_qty (string, only when the store shows quantities), rating: {average,
count}}` and `Price` = `{currency, list_price, price, discount_percent, price_with_tax | null}`
(`price` after automatic pricing rules for one unit; signed-in customers get their own;
`price_with_tax` when the store displays prices with tax). Errors: `404` unknown category,
`422` bad sort/limit/price/cursor.

**`GET /items/{slug}`** → `{slug, title, sku, short_description, description (markdown —
sanitize when rendering), images: [{url, alt}], categories: [{slug, name}], price: Price | null,
purchasable, in_stock, available_qty, unit: {name, whole_number}, max_qty, seo: {title,
description}, rating: {average, count}, reviews: [Review] (latest 10), reviews_next_cursor,
related: [Card]}`. `404` when the item is missing or unpublished.

**`GET /items/{slug}/reviews?cursor=&limit=`** (limit ≤ 50) → `{reviews: [Review], next_cursor}`,
`Review` = `{rating, title, review, reviewer_name, verified_purchase, date}` — approved reviews
only, newest first; reviewer emails are never shown.

**`POST /items/{slug}/reviews`** `{rating: 1-5, title?, review?, name, email}` (signed in: the
user's email and name) → `201 {status: "Pending", message}`. Held for moderation; *verified
purchase* is set when that email (or the signed-in customer) bought the item. Errors: `422`
rating/name/email, `409` this email already reviewed the item, `404` item.

### Cart

**`POST /cart`** `{items?: [{item: slug | website item id, qty?: "1"}]}` (≤ 100) →
`201 {token, cart: Cart}`. The token is shown once (only its hash is stored); send it as
`X-Cart-Token` from now on. Signed-in callers' carts are priced for their customer account.

`Cart` = `{status: Open | Ordered | Expired, currency, items: [{id, item (slug), title, image, available,
qty, list_price, price, discount_percent, amount, in_stock, available_qty}], item_count,
coupon: {code} | null, shipping: {id, label, amount} | null, email, phone, name, notes,
shipping_address: Address, billing_same_as_shipping, billing_address: Address,
prices_include_tax, totals: {subtotal, discount, shipping, taxes: [{description, amount}],
total_taxes, grand_total, rounding_adjustment, rounded_total}, missing: [...]}` with
`Address` = `{line1, line2, city, state, postal_code, country}` (country: ISO alpha-2).
`missing` lists what checkout still needs: `items`, `email`, `name`, `shipping_address`,
`billing_address`, `shipping_method`. On a line, `available` is false when the item was
unpublished since (checkout refuses it; remove the line) and `in_stock` is false when its
quantity cannot be supplied.

Every cart call below returns the updated `Cart`:

| Call | Body | Notes and errors |
|---|---|---|
| `GET /cart` | — | `404` unknown token |
| `POST /cart/items` | `{item, qty?: "1"}` | Adds to an existing line of the item. `422` `item` (unknown, unpublished, no price), `qty` (≤ 0, above the item's or store's limit, fractional for a whole-number unit, beyond available stock when the store enforces stock), `items` (too many lines) |
| `PUT /cart/items/{line}` | `{qty}` | `qty: 0` removes; same checks; `404` unknown line |
| `DELETE /cart/items/{line}` | — | |
| `PUT /cart/details` | any of `{email, phone, name, notes, shipping_address, billing_address, billing_same_as_shipping}` | Keys left out stay; an address replaces the previous one (null clears); giving a `billing_address` turns off *same as shipping*. `422` unknown keys, invalid email/phone, lengths, `shipping_address.country` not shipped to |
| `PUT /cart/coupon` | `{code}` | `422` `coupon_code`: not valid, not yet valid, expired, used up, already used by this customer/email, below its minimum order, or not lowering this cart |
| `DELETE /cart/coupon` | — | |
| `GET /cart/shipping-options` | — | `{options: [{id, label, description, amount}], selected}` for the cart's country, subtotal and weight |
| `PUT /cart/shipping` | `{shipping_method: id}` | `422` not available for this cart; when exactly one method applies it is chosen automatically, and a method that stops applying is dropped |

Changing an ordered or expired cart is `409`.

### Checkout

**`POST /checkout`** — headers `X-Cart-Token`, **`Idempotency-Key`** (8–200 visible ASCII
characters; or `idempotency_key` in the body); body `{payment_method: gateway, accept_terms
(required when the store has a terms page), expected_total? (the rounded total the customer
saw), return_url? (must start with the store URL), details?: {same as PUT /cart/details}}` →
`{order: Order, order_token, payment_request: PaymentRequest | null}` (`null` when nothing is
to be paid).

It re-prices the cart for the customer, re-checks every line (published, priced, within limits,
in stock — under per-item locks, so two checkouts cannot both take the last unit) and the
coupon, then creates a **submitted sales order** priced exactly like the cart and a
**payment request**. The customer: a signed-in user's linked customer (or a new one with their
email); a **guest always gets a new guest customer**, even when a customer with that email
exists — a guest never attaches to (or sees) another customer's account. The same key returns
the same result again; another key on an ordered cart is `409`. Errors: `422` (missing
details, `payment_method`, `accept_terms`, `return_url`, `shipping_address.country`, `items`
rows no longer available / out of stock, `coupon_code`), `409` (`expected_total` differs —
the message has the new total —, cart already ordered or expired), `403` guest checkout
disabled. Keep `order_token`: it is the guest's key to the order.

`Order` = `{number, date, status (sales order status), delivered_percent, payment_status
(Pending | Paid | Failed | Expired | Cancelled | Not Required), currency, email,
shipping_address, billing_address (multi-line text), notes, items: [{item (slug), title, qty,
price, amount, delivered_qty}], shipping: {method, amount}, totals: {subtotal, shipping, taxes,
total_taxes, grand_total, rounding_adjustment, rounded_total}, payments: [PaymentRequest]}`.

`PaymentRequest` = `{id, status (Initiated | Paid | Failed | Cancelled | Expired), amount,
currency, gateway, label, instructions, payment_url (while Initiated, once the gateway set it),
expires_at, paid_at}`.

### After checkout

| Call | Who | Body / headers → response |
|---|---|---|
| `GET /payment-requests/{id}` | anyone with the order token | `X-Order-Token` → `PaymentRequest`. Poll it after checkout until `payment_url` appears, then redirect the shopper there. `404` for any mismatch |
| `POST /orders/lookup` | guest | `{number, email, token}` → `{order: Order}`; `404` when any of the three is wrong |
| `POST /orders/pay` | guest | `{number, email, token, payment_method, return_url?}` → `{payment_request}`: pay again after a failed or expired payment, or switch method (an open request of the same method is returned; one of another method is cancelled). `422` paid, closed or cancelled order |
| `GET /orders?cursor=` | signed in | `{orders: [{number, date, status, payment_status, web_order, currency, grand_total}], next_cursor}` — 20 per page, newest first; `401` when not signed in |
| `GET /orders/{number}` | signed in | `{order: Order}`; `404` unless it is the user's |
| `POST /orders/{number}/pay` | signed in | as `/orders/pay` |
| `POST /orders/{number}/reorder` | signed in | `201 {token, cart: Cart, skipped: [titles]}` — a new cart with the order's goods at today's prices |
| `GET /invoices?cursor=` | signed in | `{invoices: [{number, date, due_date, status, currency, grand_total, outstanding}], next_cursor}` |

A signed-in user's customers are those whose email is the user's (on the customer, or on a CRM
contact of the customer when the CRM package is installed) — never guest customers, whose
email nobody verified.

## Gateway plugin contract

A payment gateway is a Ferrum plugin (see `docs/plugins.md`). Its **name** is what store
settings use as the payment method's `gateway`.

**Manifest.**

```json
{
  "name": "stripe",
  "permissions": [{"entity": "PaymentRequest", "read": true, "write": true}],
  "subscriptions": ["PaymentRequest.submitted"],
  "routes": [{"path": "/webhook", "public": true}],
  "http_hosts": ["api.stripe.com"],
  "config": {"secret_key": "…", "webhook_secret": "…"}
}
```

**1. A request is created.** Every checkout (and every *pay again*) submits a payment request;
the outbox delivers `PaymentRequest.submitted` with `event.doc`:
`{id, number, gateway, status: "Initiated", amount (decimal string), currency_code (ISO),
currency (id), sales_order, order_number, customer, payer_name, payer_email, description,
return_url, expires_at}`. Ignore requests whose `gateway` is not yours. Delivery is at least
once: create the provider session idempotently (e.g. with the request `id` as the provider's
idempotency key).

**2. Hand over the checkout link.** Call `data.action` with
`{"entity": "PaymentRequest", "id": "<id>", "action": "set_gateway_details",
"params": {"payment_url": "https://…", "gateway_reference": "cs_…"}}`. The storefront polls the
request and redirects the shopper to `payment_url` (https only). Refused once the request is no
longer open.

**3. Confirm the payment** (from the provider's webhook on your public route, after verifying
its signature — or by polling the provider in a schedule):

```json
{"entity": "PaymentRequest", "id": "<id>", "action": "mark_paid",
 "params": {"amount": "1149.88", "currency": "INR", "gateway_reference": "pi_…"}}
```

→ `{status: "Paid", payment_entry, payment_entry_number, replayed}`. It books and submits a
Payment Entry (receive, customer, the method's account, reference = `gateway_reference`),
allocated to the order's open invoices or kept as an advance; sets the request `Paid` and the
order's web payment status; notifies Website Managers; emits `commerce.order_paid`.

* **Idempotent:** confirming a paid request again returns the same entry with
  `replayed: true` — one payment entry, whatever the retries.
* **Refused:** another `amount` or `currency` than requested (`422`), an expired or cancelled
  request or a cancelled order (`422` — refund the payment), a second payment with a different
  `gateway_reference` (`409` — refund it), another plugin's request (`403`). A plugin may
  settle only requests whose `gateway` is its own name, and never `Manual` / `Cash on Delivery`.
* A `Failed` request can still be paid while not expired (a retry on the provider's page).

**4. Report a failure:** action `mark_failed`, params `{reason?, gateway_reference?}` → the
request becomes `Failed` (the order shows it; the shopper can pay again), emits
`commerce.payment_failed`. Ignored for requests that are no longer open; `409` for paid ones.

Staff with write access to payment requests (Website, Accounts and Sales managers, Accounts
users) use the same `mark_paid` action for manual and cash-on-delivery payments.

**Expiry.** Requests expire `expiry_minutes` after creation. Every minute, overdue open or
failed requests become `Expired` (`commerce.payment_expired`) and, when the store cancels unpaid
orders, the order is cancelled — releasing its stock — unless it was paid or has a newer open
request. Expired requests are never paid: refund late captures.

**Refunds.** Cancel the payment entry (refund at the provider); the request then shows
`Cancelled` and the order can be cancelled. A paid web order cannot be cancelled before that.

## Events for mailers

Besides the usual document events, the outbox carries (for a comms package, webhooks or
automations): `commerce.order_placed` `{sales_order, number, customer, email, name, currency,
grand_total, payment_method, payment_request, order_token}` — the token belongs in the
confirmation email (the order page needs number + email + token), so treat these payloads as
confidential —; `commerce.order_paid`, `commerce.payment_failed`, `commerce.payment_expired`
and `commerce.cart_abandoned` `{cart, email, name, item_count, grand_total, currency}` (carts
with an email that went idle).

## Stock

Sellable quantity = stock on hand in the store's warehouses − what open (submitted, not
closed, not fully delivered) sales orders still have to deliver from them. So every order —
web or staff — reserves its stock until delivered, closed or cancelled; an unpaid web order
reserves until its payment request expires (when the store cancels unpaid orders). With
*enforce stock*, carts and checkout refuse quantities beyond it; non-stock items never run out.

## Reports

`web_sales` (by day, item or category), `top_products`, `abandoned_carts` (value, items,
contactable), `conversion_funnel` (carts created → with items → checked out → paid) and
`coupon_usage` — under *Commerce* in the reports list; revenue in the company currency.

## Upkeep

Every minute (one replica at a time): expire overdue payment requests (cancelling unpaid orders
as configured), mark carts with items idle for *abandon after hours* as `Abandoned` (a returning
shopper revives them) and carts idle for *cart expiry days* as `Expired` (frozen).

## Known limits

* One store per tenant; prices come from one price list read as company-currency prices (the
  selling engine converts them to a foreign store currency at the day's rate).
* Item variants and attributes are not modelled by `Item`, so the store has none; a default
  territory is not set on customers (core customers have no territory).
* Expired carts are kept (with their contact details) until deleted by staff.
* Advances in another currency at a different booked rate are left for the accountant to
  allocate (an exchange difference needs a journal entry).
