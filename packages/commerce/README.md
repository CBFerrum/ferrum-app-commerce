# Commerce — the online store

Sell online from the same items, prices, stock and books as the rest of Ferrum. Package
`commerce` (depends on `core`, `selling`, `inventory`, `accounting`); logic in
`crates/pkg-commerce`.

* **Catalog** — `ItemCategory` (a tree), `WebsiteItem` (the published face of an `Item`: slug,
  title, descriptions, images, categories, related items, SEO, ranking, weight, order limit) and
  moderated `ItemReview`s. A `WebsiteItem` can be a **variant** of another (`variant_of`, plus
  `options`: Size M, Colour Red): see "Variants".
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

* **Subscriptions** (the connector app Store Subscriptions, package `commerce_subscriptions`, with
  Finance) — a `WebsiteItem` with `csub_plan` is sold as a Finance `SubscriptionPlan`. Its web order
  pays the first period; the `Subscription` it starts (`csub_sales_order`) is billed by Finance from
  the second. Each renewal invoice gets a `PaymentRequest` with `sales_invoice` through the order's
  payment method. Portal: `GET subscriptions`, `POST subscriptions/{number}/pay` (`invoice`,
  `payment_method`) and `POST subscriptions/{number}/cancel` (at the end of the period paid for).
  `ferrum_app_commerce::entitled(tx, tenant, customer, plan)` says whether a customer has it.

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

`Card` = `{slug, title, short_description, image: {url, alt} | null, price: Price, from (true
for an item with variants: the price is its cheapest variant's),
in_stock, available_qty (string, only when the store shows quantities), rating: {average,
count}}` and `Price` = `{currency, list_price, price, discount_percent, price_with_tax | null}`
(`price` after automatic pricing rules for one unit; signed-in customers get their own;
`price_with_tax` when the store displays prices with tax). Errors: `404` unknown category,
`422` bad sort/limit/price/cursor.

**`GET /items/{slug}`** → `{slug, title, sku, short_description, description (markdown —
sanitize when rendering), images: [{url, alt}], categories: [{slug, name}], price: Price | null,
purchasable, in_stock, available_qty, unit: {name, whole_number}, max_qty, seo: {title,
description}, rating: {average, count}, reviews: [Review] (latest 10), reviews_next_cursor,
related: [Card], variant_of: {slug, title, published} | null, options: [{option, value}],
variants: [{slug, title, options, price, purchasable, in_stock, available_qty, image}]}`.
An item with variants is not `purchasable` itself (choose a variant: adding it to a cart is
refused); a variant says which item it is one of. `404` when the item is missing or unpublished.

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
qty, list_price, price, discount_percent, promotion_percent, added_by_promotion, amount, in_stock, available_qty}], item_count,
coupon: {code} | null, codes: [{code, applied}], promotions: [{title, amount}], shipping: {id, label, amount} | null, email, phone, name, notes,
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
| `PUT /cart/coupon` | `{code}` | The store's own coupon (one per cart), or one of ERP's promotion codes (up to ten): promotions price the cart as any sales order (amount or percent off the order or lines, buy and get, bundles, free delivery on the shipping item, free items added as lines with `added_by_promotion`). `422` `coupon_code`: not valid (an unknown and a used-up promotion code get the same answer), not yet valid, expired, used up, already used by this customer/email, below its minimum order, or not lowering this cart |
| `DELETE /cart/coupon` | — | Takes the store's coupon off |
| `DELETE /cart/codes/{code}` | — | Takes that code off (a promotion code or the coupon) |
| `GET /cart/shipping-options` | — | `{options: [{id, label, description, amount}], selected}` for the cart's country, subtotal and weight |
| `PUT /cart/shipping` | `{shipping_method: id}` | `422` not available for this cart; when exactly one method applies it is chosen automatically, and a method that stops applying is dropped |

Changing an ordered or expired cart is `409`.

### Checkout

**`POST /checkout`** — headers `X-Cart-Token`, **`Idempotency-Key`** (8–200 visible ASCII
characters; or `idempotency_key` in the body); body `{payment_method: gateway, accept_terms
(required when the store has a terms page), expected_total? (the rounded total the customer
saw), return_url? (must start with the store URL), details?: {same as PUT /cart/details},
gift_card?: {code, pin?}}` →
`{order: Order, order_token, order_link, payment_request: PaymentRequest | null}` (`null` when
nothing is to be paid). `order_link` is the shopper's tracking link (see "After checkout"), made when the
store has a storefront URL.

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

With `gift_card`, the store's Gift Cards kind pays what it holds of the order (ERP's stored value, which
pays invoices: the order is invoiced at checkout, and the card is redeemed through ERP's prove step, its
code and PIN never kept); `payment_method` pays the rest and may be left out when the card covers it all.
`payment_request` is then the rest's request; the card's own request (`gateway: "Gift Card"`) is already
paid. `422` `gift_card` (unknown code, wrong PIN, blocked or used up; ERP's guessing limits apply).
An item that names a stored value kind sells a gift card: once its order is paid, the card is opened and
its code (and PIN) go to the shopper once in the `commerce.gift_card_issued` message.

`Order` = `{number, date, status (sales order status), delivered_percent, payment_status
(Pending | Paid | Failed | Expired | Cancelled | Not Required | Partly Refunded | Refunded),
payment_method, steps: [{key, label, done}] (placed, paid, shipped), shipments: [{date, carrier,
tracking_number, tracking_url}], refunds: [{status (Approved | Refunded), kind, amount, currency,
date}], currency, email,
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
| `POST /orders/track` | anyone with the tracking link | `{number, token}` → `{order: Order}` without `email`, `billing_address`, `notes` and the street: `ship_to` names only the town and country. `404` for any mismatch; a cancelled order answers, with its status |
| `POST /orders/pay` | guest | `{number, email, token, payment_method, return_url?}` → `{payment_request}`: pay again after a failed or expired payment, or switch method (an open request of the same method is returned; one of another method is cancelled). `422` paid, closed or cancelled order |
| `GET /orders?cursor=` | signed in | `{orders: [{number, date, status, payment_status, web_order, currency, grand_total}], next_cursor}` — 20 per page, newest first; `401` when not signed in |
| `GET /orders/{number}` | signed in | `{order: Order}`; `404` unless it is the user's |
| `POST /orders/{number}/pay` | signed in | as `/orders/pay` |
| `POST /orders/{number}/reorder` | signed in | `201 {token, cart: Cart, skipped: [titles]}` — a new cart with the order's goods at today's prices |
| `GET /invoices?cursor=` | signed in | `{invoices: [{number, date, due_date, status, currency, grand_total, outstanding}], next_cursor}` |

**The tracking link** is `{store_url}/order#n={number}&t={order_token}`. The token is in the address'
*fragment*, which browsers never send, so no server, log or `Referer` sees it; the order page reads it and
calls `/orders/track`. It comes with the checkout result (`order_link`), in the `commerce.order_placed` and
`commerce.order_shipped` events, and from the desk (the `web_tracking_link` action on a web order). Anyone
who has the link can follow the order, so mailers should treat it as confidential.

A signed-in user's customers are those whose email is the user's (on the customer, or on a CRM
contact of the customer when the CRM package is installed) — never guest customers, whose
email nobody verified.

## Payment gateways

Three gateways come with this repository (`gateways/`), built as plugins on the contract below:

| Gateway | Shoppers pay on | Settings | Webhook events to send |
|---|---|---|---|
| `razorpay` | a Razorpay Payment Link (UPI, cards, netbanking, wallets) | `key_id`, `key_secret`, `webhook_secret` | `payment_link.paid`, `payment_link.expired`, `payment_link.cancelled` |
| `paypal` | PayPal (PayPal balance, cards, Pay Later); captured when the buyer approves | `client_id`, `client_secret`, `webhook_id` (the webhook's ID at PayPal: letters and digits), `environment` (`live` or `sandbox`) | `CHECKOUT.ORDER.APPROVED`, `PAYMENT.CAPTURE.COMPLETED`, `PAYMENT.CAPTURE.DENIED` |
| `stripe` | a Stripe Checkout page (cards, wallets, bank debits) | `secret_key`, `webhook_secret`, `success_url` (used when an order has no https return URL) | `checkout.session.completed`, `checkout.session.async_payment_succeeded`, `checkout.session.async_payment_failed`, `checkout.session.expired` |

1. Build them: `cd gateways && cargo build --release --target wasm32-unknown-unknown`.
2. Install one as an administrator: `POST /api/admin/plugins` with its `manifest.json` (fill in
   `config`) plus `wasm_base64` of `target/wasm32-unknown-unknown/release/<gateway>.wasm`; or set
   the settings later with `PUT /api/admin/plugins/{id}/config`.
3. Point the provider's webhook at `https://<your host>/api/public/<workspace>/<gateway>/webhook`
   with the events above, and put its signing secret in `webhook_secret`.
4. Add a payment method with that gateway (`razorpay`, `stripe` or `paypal`) to Store Settings.

Each payment request becomes one link, checkout session or order, made once however often the
event is delivered (Razorpay finds its link by the request's id; Stripe and PayPal get the
request's id as the idempotency key, and PayPal captures an order once). Webhooks are accepted only
with a valid signature (Stripe's also no older than five minutes; PayPal's checked through PayPal's
own verification call); a refusal to book (the request expired, the amount changed) answers the provider
and logs a warning to refund the payment, while a passing failure makes the provider retry.
Every five minutes a schedule asks the provider about the requests still open, so a lost webhook
delays a confirmation instead of losing it, and closes links and sessions whose request closed.

`cargo test` in `gateways/` runs each gateway against a scripted provider. Two of them are also
checked against their providers' own API definitions, which CI serves: Stripe's through stripe-mock
(`STRIPE_MOCK_URL`), which refuses unknown or malformed parameters as Stripe does, and PayPal's
orders and webhook definitions through a validating mock (`PAYPAL_ORDERS_MOCK`,
`PAYPAL_WEBHOOKS_MOCK`), which refuses a request with a field of the wrong shape (that is how it
caught webhook IDs pasted with spaces, now trimmed; an ID of another shape is reported instead of
sent). With test-mode keys in
the environment it also talks to the provider itself: `STRIPE_TEST_SECRET_KEY` (`sk_test_…`)
creates and expires a real Checkout Session, `RAZORPAY_TEST_KEY_ID` (`rzp_test_…`) with
`RAZORPAY_TEST_KEY_SECRET` creates and cancels a real Payment Link, and
`PAYPAL_SANDBOX_CLIENT_ID` with `PAYPAL_SANDBOX_CLIENT_SECRET` signs in to the sandbox and creates
an order (left unapproved, it lapses). Without them those tests say they were skipped. CI passes the
repository secrets of the same names. Live keys are refused.

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

**Refunds.** Money a gateway took and could not book is a `PaymentException` (above). Refunds of
payments that *were* booked, whole or in part, are `OrderRefund` records: see "Refunds of booked
payments". A paid web order cannot be cancelled except through a cancellation refund.

## Refunds of booked payments

A person asks for a refund from the order's page in the store's admin (or by inserting an `OrderRefund`),
and the server does the sums. Two kinds:

* **Return** (`kind: Return`, `lines: [{sales_order_line, qty, restock}]`, `include_shipping`): delivered
  and invoiced goods come back. The server checks each line against what was delivered, invoiced and
  already asked for, makes the credit note, and takes its total as `amount`; the caller never sets it. An
  order must be invoiced first (a return credits an invoice).
* **Cancellation** (`kind: Cancellation`): nothing shipped or was invoiced; the whole payment goes back.

`amount` can never pass what the payment still holds (paid, less the refunds that are asked for,
approved, failed or done). The record then moves `Requested` → `Approved` (a person, with write on
`OrderRefund`; a plugin is refused) → `Refunded` or `Refund Failed` (the gateway, or a person with
`mark_refunded` for cash, a bank transfer or a refund made from the provider's dashboard) and `Declined`
(a person). `refund_failed` can be approved again.

**What a gateway plugin does**, on its schedule, alongside `PaymentException`:

1. Permission in its manifest: `{"entity": "OrderRefund", "read": true, "write": true}`.
2. `data.query("OrderRefund", {"gateway": "<name>", "status": "Approved"}, N)`. Each row has `id`,
   `number`, `payment_reference` (the provider's own id of the payment, as given to `mark_paid` as
   `gateway_reference`), `amount` (a decimal string, **in the request's currency: whole or part, never
   assume the whole payment**) and `currency_code`.
3. Ask the provider for a refund of `amount` on `payment_reference`, with the refund's `id` as the
   idempotency key (or find a refund made earlier by it, as Razorpay's plugin does for exceptions).
4. Report with `data.action("OrderRefund", id, "refund_done", {"refund_reference": "re_…"})`, or
   `refund_failed` with `{"error": "…"}` for a refusal that will not change. Repeating either is harmless.
   Only the plugin whose name is the refund's `gateway` may; the others get `403`.

`refund_done` also does the books, in the same call and all or nothing: a return delivery note puts the
goods back (for the lines marked to restock), the credit note is submitted, the refund is posted out of
the bank the payment came into and matched to what the credit note owes, and the order is closed when
nothing is left to ship (a cancellation reverses the payment entry and cancels the order instead). If the
books refuse (a closed period, a missing account, a payment in another currency than the company's), the
call still succeeds: the refund is `Refunded`, `booked` is false and `booking_error` says why, the
managers are told, and `post_books` tries again. The gateways in `gateways/` do not carry these out yet;
until they do, a person records the refund with `mark_refunded` after sending it from the provider.

`POST /api/app/commerce/desk/refund-plan` (signed in; needs create on `OrderRefund` and read on the order)
is what the dialog asks: `{sales_order, kind?, lines?, include_shipping?}` → what may still come back per
line, the payment and what it holds, whether the order can be cancelled, and the amount the choice comes
to. It writes nothing.

## The store's desk

Three actions on web orders let the store's people (Website Manager, Sales Manager, Sales User, and
administrators) run them without the broader Sales rights: `web_ship` (one delivery note for everything
still to deliver, with `carrier`, `tracking_number`, `tracking_url`, `notify` and `invoice`, submitted,
and the invoice that applies the shopper's payment; all or nothing), `web_invoice` and
`web_tracking_link`. `DeliveryNote` carries the tracking fields and a *Tell the Customer* switch, and
`SalesOrder` the *Web Payment Method*. A queue, per order, is read from `status`, `per_delivered`,
`per_billed`, the web payment status and method; the admin's rules for it are in `ui/src/queues.ts`.

## After sales (0.9.0)

* **Cancel Web Order** (desk) and the shopper's **Cancel My Order** make a cancellation refund for each payment of an order
  nothing of which shipped (a gift card's first: it is given back last, once the provider's part has gone). **Update Web
  Order** changes lines and address through ERP's Update Items and settles the difference through the payment (a new
  request, or an `Adjustment` refund, or an amount recorded as owed). **Return Something** is a `ReturnRequest` the desk
  approves or declines; **Receive Goods** makes the refund. All three of the shopper's links come from the order
  confirmation (a signed action link); Store Settings' After Sales tab holds the windows, the fee and where refunds go.
* A gift card that covers part of an order **holds** that part (ERP's stored value hold) until the rest is paid.
* Every mail to the shopper is a message template (`commerce.*`), editable in the workspace.
* **Generate Variants** on a website item makes ERP's item variants (Stock Operations) and their website items.

## Variants

`Item` has no variants, so a variant here is a `WebsiteItem` of its own, with its own `Item` (price, stock,
code), whose `variant_of` names the website item it is a variant of and whose `options` say what sets it
apart (`Colour` = `Red`, `Size` = `M`). Rules: variants go one level deep; every variant of an item names
the same options; no two have the same choices; an item with variants is sold only through them. Cart,
checkout, stock and orders need nothing more: a variant is an item, named "Logo Tee — Red / M" by default.
The storefront lists the parent once ("From" its cheapest published variant, in stock when one is), the
variant's address redirects to the parent's page with `?v=` choosing it, and search takes each word of a
query anywhere in an item, its variants and their choices. Reviews of the parent are verified by a
purchase of any variant.

## Events for mailers

Besides the usual document events, the outbox carries (for a comms package, webhooks or
automations): `commerce.order_placed` `{sales_order, number, customer, email, name, currency,
grand_total, payment_method, payment_request, order_token}` — the token belongs in the
confirmation email (the order page needs number + email + token, or the `order_link` alone), so treat
these payloads as confidential —; `commerce.order_shipped` `{sales_order, number, delivery_note, email, carrier, tracking_number,
tracking_url, notify, order_link, ship_to}` (`notify` is the *Tell the Customer* switch),
`commerce.order_refunded` `{sales_order, number, refund, kind, amount, currency, gateway, email, notify}`,
`commerce.order_paid`, `commerce.payment_failed`, `commerce.payment_expired`
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
* Item variants are not modelled by `Item`: each variant is a store item of its own (see "Variants"), made
  one by one; there is no generator for all the combinations of a size and a colour. A default territory
  is not set on customers (core customers have no territory).
* Reports count orders as they were placed: refunds are not netted from `web_sales` or `top_products`.
* A refund is booked in the company's currency only (a payment in another currency is refunded, then
  posted by hand), and an order must be invoiced before goods can be returned. A tax template with fixed
  (`Actual`) charges is left out of a partial credit note.
* Expired carts are kept (with their contact details) until deleted by staff.
* Advances in another currency at a different booked rate are left for the accountant to
  allocate (an exchange difference needs a journal entry).
