<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/assets/banner-dark.svg">
  <img alt="Ferrum Commerce: an online store on your own catalog, stock and books" src="docs/assets/banner-light.svg" width="100%">
</picture>

<br>

[![Version](https://img.shields.io/badge/version-0.10.4-be185d?style=flat-square)](CHANGELOG.md)
[![Licence: AGPL-3.0](https://img.shields.io/badge/licence-AGPL--3.0-blue?style=flat-square)](LICENSE)
[![Ferrum platform](https://img.shields.io/badge/runs%20on-Ferrum-2f5bea?style=flat-square)](https://github.com/CBFerrum/Ferrum)
![Rust](https://img.shields.io/badge/Rust-server-b7410e?style=flat-square&logo=rust)
![PostgreSQL](https://img.shields.io/badge/PostgreSQL-data-336791?style=flat-square&logo=postgresql&logoColor=white)
![React](https://img.shields.io/badge/React%20%2B%20TypeScript-interface-0ea5e9?style=flat-square&logo=react&logoColor=white)
![Tests](https://img.shields.io/badge/tests-66%20Rust%20%2B%2016%20UI%20files-16a34a?style=flat-square)

### Sell online from the items, prices, stock and books you already keep in Ferrum.

Ferrum Commerce is for businesses that want a web store whose orders are ERP sales orders from the first click.
It replaces a hosted shop that has to be connected to stock and accounting: carts are priced and taxed by the
same engine that invoices, and a refund leaves credit notes and bank entries behind it.

[💡 Why](#-why-ferrum-commerce) · [📸 Screenshots](#-screenshots) · [✨ Features](#-features) · [📊 Compare](#-how-it-compares) · [📍 Roadmap](#-roadmap) · [🚀 Install](#-install) · [🔧 Develop](#-develop)

</div>

## 💡 Why Ferrum Commerce

- 🧬 **The shop is your ERP.** Items, customers, price lists, stock, sales orders, invoices and payments are ERP's records. There is nothing to sync.
- 🧮 **One price from cart to invoice.** Every change re-prices the cart with the selling engine (price list, promotions, tax, rounding), so cart, order and invoice agree to the cent.
- ↩️ **Refunds that reach the books.** A return restocks goods, credits the invoice, posts the money out of the bank and matches it, all or nothing.
- 🔌 **Payments by plugins.** Stripe, PayPal and Razorpay ship as sandboxed WebAssembly plugins; the plugin host holds the keys, not the app.
- 🏠 **Self-hosted and open source.** AGPL-3.0, no revenue share and no transaction fee from Ferrum. You pay your payment provider.
- 🌐 **A storefront on your own domain,** rendered on the server so search engines and link previews read it; a documented API for your own front end.
- 🌍 **English, Arabic (right to left) and Hindi** in the admin; the storefront's words are translatable per language.

## 🎯 At a glance

| | |
|---|---|
| **Version** | 0.10.4 (packages: `commerce` 0.5.0, Store Subscriptions connector 0.1.0) |
| **Kind** | Its own interface at `/commerce/` (today, orders desk, returns, money, stock alerts, variants, storefront words) plus a server-rendered storefront and Ferrum's standard lists and forms |
| **What it adds** | 29 entities (14 records and 15 line tables), 5 roles, 5 reports, 16 message templates, 1 menu, 2 create shortcuts, 3 payment gateway plugins |
| **Depends on** | Ferrum platform `>=0.3.6, <0.4`; ERP `>=0.24` (installing Commerce installs ERP) |
| **Works with** | ERP (items, customers, stock, orders, invoices, promotions, gift cards), Finance (subscriptions), Stock Operations (variants), Bookings (deposits), Retail (returns at a till), Campaigns and Mail (events and messages) |
| **Languages** | Admin in English, Arabic, Hindi; storefront words English with per-language overrides |
| **Tests** | 66 Rust tests against a real PostgreSQL, 16 interface test files, gateway tests against scripted and mock providers |
| **Licence** | AGPL-3.0-only |

## 📸 Screenshots

<picture><source media="(prefers-color-scheme: dark)" srcset="docs/assets/screens/commerce-today-dark.webp"><img alt="Store — today" src="docs/assets/screens/commerce-today-light.webp" width="100%"></picture>

<p align="center"><b>☀️ Today</b><br><sub>See today's web orders, what to pay, ship and invoice, and the day's sales.</sub></p>

<table>
<tr>
<td width="50%" valign="top">
<picture><source media="(prefers-color-scheme: dark)" srcset="docs/assets/screens/commerce-orders-dark.webp"><img alt="Store — orders" src="docs/assets/screens/commerce-orders-light.webp"></picture>
<p><b>🧾 Orders desk</b><br><sub>Work through web orders from payment to shipping and invoice.</sub></p>
</td>
<td width="50%" valign="top">
<picture><source media="(prefers-color-scheme: dark)" srcset="docs/assets/screens/commerce-money-dark.webp"><img alt="Store — money" src="docs/assets/screens/commerce-money-light.webp"></picture>
<p><b>💳 Money</b><br><sub>See payments, refunds and exceptions for the online store.</sub></p>
</td>
</tr>
<tr>
<td width="50%" valign="top">
<picture><source media="(prefers-color-scheme: dark)" srcset="docs/assets/screens/commerce-website-item-list-dark.webp"><img alt="Website items — list" src="docs/assets/screens/commerce-website-item-list-light.webp"></picture>
<p><b>🛍️ Website items</b><br><sub>Browse, search and filter what the store shows of your items.</sub></p>
</td>
<td width="50%" valign="top">
<picture><source media="(prefers-color-scheme: dark)" srcset="docs/assets/screens/commerce-coupon-code-list-dark.webp"><img alt="Coupon codes — list" src="docs/assets/screens/commerce-coupon-code-list-light.webp"></picture>
<p><b>🎟️ Coupon codes</b><br><sub>Browse, search and filter coupon codes.</sub></p>
</td>
</tr>
<tr>
<td width="50%" valign="top">
<picture><source media="(prefers-color-scheme: dark)" srcset="docs/assets/screens/commerce-shipping-rule-list-dark.webp"><img alt="Shipping rules — list" src="docs/assets/screens/commerce-shipping-rule-list-light.webp"></picture>
<p><b>🚚 Shipping rules</b><br><sub>Browse the delivery methods and the places each reaches.</sub></p>
</td>
<td width="50%" valign="top">
<picture><source media="(prefers-color-scheme: dark)" srcset="docs/assets/screens/commerce-website-item-new-dark.webp"><img alt="Website item — new" src="docs/assets/screens/commerce-website-item-new-light.webp"></picture>
<p><b>➕ New website item</b><br><sub>Fill in the form to add an item to the store.</sub></p>
</td>
</tr>
<tr>
<td width="50%" valign="top">
<picture><source media="(prefers-color-scheme: dark)" srcset="docs/assets/screens/commerce-report-web-sales-dark.webp"><img alt="Web sales — report" src="docs/assets/screens/commerce-report-web-sales-light.webp"></picture>
<p><b>📊 Web sales report</b><br><sub>Run web sales and read the result by day, item or category.</sub></p>
</td>
<td width="50%"></td>
</tr>
</table>

<table>
<tr>
<td align="center" valign="top">
<picture><source media="(prefers-color-scheme: dark)" srcset="docs/assets/screens/commerce-today-phone-dark.webp"><img alt="Store — today" src="docs/assets/screens/commerce-today-phone-light.webp" width="260"></picture>
<p><b>📱 Today (phone)</b><br><sub>See today's web orders, what to pay, ship and invoice, and the day's sales.</sub></p>
</td>
<td align="center" valign="top">
<picture><source media="(prefers-color-scheme: dark)" srcset="docs/assets/screens/commerce-website-item-list-phone-dark.webp"><img alt="Website items — list" src="docs/assets/screens/commerce-website-item-list-phone-light.webp" width="260"></picture>
<p><b>📱 Website items (phone)</b><br><sub>Browse, search and filter what the store shows of your items.</sub></p>
</td>
</tr>
</table>

## ✨ Features

### 🛍️ Catalog

- **Store settings in one record.** Store name, company, currency, price list, tax template, whether prices show with tax, warehouses and the fulfilment warehouse, stock enforcement and whether quantities show, guest checkout, default customer group, the shipping item, allowed countries, payment methods, terms link, store URL, order limits, abandonment and expiry times.
- **Categories as a tree.** An Item Category has a name, image, description, SEO fields and a published flag; an unpublished category hides its branch.
- **Website items.** The published face of an ERP item: slug, title, short and long description, images with alt text, categories, hand-picked related items, weight, a per-item order limit, SEO title and description, ranking, and rating average and count.
- **Variants.** A size or a colour is an item of its own with its own price, stock, code and picture. **Generate Variants** takes option sets such as `{"Size": ["S","M"], "Colour": ["Red"]}`, makes the attributes and combinations in ERP's Stock Operations, and gives each a website item. The parent is listed once, "From" its cheapest variant; its page tells which combinations are sold out or not offered together.
- **Reviews.** A shopper's review (1 to 5 stars, title, text, name, email) is held as Pending until a manager approves it; verified purchase is set when that email bought the item; one review per email per item; reviewer emails are never shown.
- **Back-in-stock alerts.** A shopper leaves an email on an out-of-stock item and is told once, when it is back.
- **Import and export.** Website items, categories and coupons import and export as CSV.
- **Server-rendered pages without scripts.** Product pages work without scripts; the cart needs them. Names, descriptions and reviews are escaped, and images are the workspace's public files or HTTPS addresses.

### 🛒 Storefront, cart and checkout

- **Pages** at the root of a `site:commerce` host: home and categories a page at a time, search, product pages with images, price (with or without tax), stock, description, reviews, related items, schema.org Product data and Open Graph, `robots.txt` and `sitemap.xml`. Until the store has its settings the site says it opens soon.
- **Search and sort.** Search finds each word anywhere in an item, its variants and their choices. Sort by ranking, price, newest, name or rating; filter by category, price range and in stock only, kept in the address.
- **Cart.** Quantities, removing, steppers, and a placeholder while it loads. The cart asks where it is going (country, region, postal code), lists the delivery methods for the place and shows the total for it, so the total shown is the total charged. A cart's token is shown once and only its hash is stored.
- **Priced on every change.** Price list, promotions, tax and rounding come from the selling engine; clients choose items and quantities, never prices or totals.
- **Checkout in one transaction.** Customer, contact and addresses, final pricing, a stock re-check, a submitted ERP sales order and a payment request, idempotent per cart and client key. Two named steps, a folded summary, each refused field marked beside it.
- **Guest and signed-in checkout.** A guest gives an email; a signed-in portal customer gets their own prices and orders.
- **Tax follows the delivery address.** Checkout records the shipping address as a structured Address (country, region, postcode), which ERP's country rules read; the store's tax template is the fallback.
- **On a phone.** Empty categories, no results and nothing in stock each say what to do next, and every view has a failure with a way on.
- **Languages.** Every storefront word is a key. English ships; a **Storefront Words** record per language overrides any word, Store Settings' Languages Offered picks the default, and the visitor's choice (`?lang=`) or browser language selects it. Right-to-left languages get `dir="rtl"`, and numbers and country names follow the page's language.
- **Order tracking without a sign-in.** A link (the order number and token sit in the address fragment, which no server or log sees) opens a page with the steps, the carrier and the parcel, refunds on their way, the lines, and only the town it goes to.

### 🎟️ Promotions, coupons and gift cards

- **Promotions through ERP.** Amount or percent off the order or some lines, buy and get, bundles, free delivery, free items (added as lines), automatic or by code, several codes at once (up to ten a cart), combining as each promotion says. Every code on the cart is listed with what it took and which did not apply.
- **Coupon codes.** A Coupon Code unlocks a pricing rule with validity dates, a minimum order, a total limit and a per-customer limit; the rule applies to nothing else.
- **Gift cards and store credit.** At checkout a gift card pays what it holds and the rest goes by the chosen method. A card that covers part of an order holds that part until the rest is paid. The code is never kept. A refund can go back as store credit; gift card and store credit codes are mailed once.

### 🚚 Shipping and stock

- **Shipping rules as zones.** A rule is a fixed charge or bands by order amount or total weight, limited to countries, regions and postal ranges, with a free-over threshold. The charge is a service line, so it is taxed, invoiced and booked like any other.
- **Stock that counts orders.** Sellable quantity is stock on hand in the store's warehouses less what open orders still have to deliver. An unpaid web order reserves stock until its payment request expires. Enforcing stock refuses quantities beyond it; non-stock items never run out.
- **Shipped in parts, from several warehouses.** Each line is reserved in the warehouse that holds it. Ship Web Order takes lines, part of a line, and a warehouse for each, with carrier, tracking number and link, whether to tell the shopper, and whether to invoice at the same time.
- **Shipment news.** Shipped, out for delivery and delivered mails; the upkeep reads ERP's carrier shipments to tell the shopper once each.

### 💳 Payments

- **Payment requests.** A request asks to pay a web order or an invoice, in whole or in part (a deposit). Status Initiated, Paid, Failed, Cancelled or Expired, with expiry, payment link, gateway reference and failure reason.
- **Three gateways ship,** as WebAssembly plugins: Stripe (Checkout Session, signed webhook), PayPal (orders captured when the buyer approves, webhook verified through PayPal) and Razorpay (Payment Link: UPI, cards, netbanking, wallets). Each request becomes one link or session, once however often the event arrives; a five-minute reconcile asks the provider about open requests so a lost webhook delays rather than loses a payment.
- **Manual methods.** Bank transfer and Cash on Delivery are payment methods too; a person marks them paid.
- **Booked at once.** A confirmed payment becomes an ERP payment entry allocated to the order's open invoices, otherwise an advance applied when the invoice is submitted.
- **Payment exceptions.** Money a gateway took but could not book (the request had closed, the amount changed) is a Payment Exception that a manager sees, with a refund the gateway carries out once however often it is retried.
- **Pay the rest.** A shopper whose gift card covered part and whose card failed can pay what is left.
- **Subscriptions** (Store Subscriptions connector with Finance): a website item can be sold as a Finance plan; the web order pays the first period and Finance bills the rest. Each renewal gets a payment request through the order's method; signed-in customers see their subscriptions in the portal, pay a renewal, or cancel (running to the end of the period paid for). Other apps ask whether a customer is entitled to a plan.

### 🧾 The orders desk

- **Today.** Web sales today and over the last 7 and 30 days, orders to deliver, failed or expired payments, reviews to moderate and the catalog's state, with live updates.
- **Orders** sorted into what waits for money, what is to be picked and sent, and what is sent but not invoiced, longest-waiting first, each with the one next step: ship, invoice, or mark a bank transfer or cash paid.
- **A pick list** turns the orders to send into what to take off the shelves, and prints.
- **An order's page.** Items, every payment with reference and expiry, shipments with tracking, invoices, refunds, the customer, where it goes and the story so far with full times on the workspace's clock.
- **Change an order.** Quantities, lines added or removed and a new address, before it is fulfilled. The desk asks the server what saving would do and shows the lines as priced again, the totals, the difference and how it is settled: charge through a method, send back through a payment, or recorded as owed.
- **Cancel an order** that has not shipped: each payment goes back, the gift card's part first.
- **Send a mail again** (Shipped, Payment received, Payment failed, Refunded), counted and limited per day per order.
- **Website Managers run the desk** without the broader Sales roles.

### ↩️ Returns and refunds

- **Shopper self-service.** The order confirmation carries a signed link to Cancel My Order and Return Something, within windows set in Store Settings' After Sales tab: cancellation hours (24 by default), whether the cancellation refund is automatic, return days after delivery (14 by default), who pays return shipping, a restocking fee percentage, and where refunds go (original payment or store credit).
- **Return requests.** A ReturnRequest is approved or declined with a note, then Receive Goods makes the refund, with the amount less the fee shown before it is confirmed. The shopper's reason, note, the terms they were given and whether it came inside the window are on the request.
- **Refunds of four kinds.** Return, Cancellation, Payment (a part of a payment, for apps such as Bookings) and Adjustment (an order change). The server works out the amount by making the credit note, holds it to what the payment still has, and a person approves.
- **The books follow, all or nothing.** A return delivery note, a credit note, the money posted out of the bank and matched to what the credit note owes, and the order closed when nothing is left to ship. If the books refuse, the refund stays Refunded, says why, and can be posted again.
- **Gateways carry out refunds** once however often retried. Cash and bank transfers are refunded by hand and recorded.
- **The Money screen.** Refunds waiting for a decision, and payments a gateway took and could not book. A manager approves (the amount, the gateway and a warning that it sends money are on the confirmation), declines with a reason, or says it was refunded another way.
- **Return reasons** with a restock default.

### ✉️ Mail and events

- **Sixteen editable message templates,** per language: order placed, payment received, payment failed (with a link to try again), shipped, out for delivery, delivered, cancelled, refunded, order changed, return requested, return approved, return declined, back in stock, cart reminder, gift card issued and store credit issued.
- **Cart reminder.** An idle cart with an email becomes Abandoned and, where the shopper agreed at checkout (May Remind About This Cart), is reminded.
- **Events for other apps.** `commerce.order_placed`, `order_shipped`, `order_refunded`, `order_paid`, `payment_failed`, `payment_expired`, `payment_refunded`, `invoice_paid` and `cart_abandoned` go to the outbox for mailers, webhooks and automations. They carry tokens, so treat them as confidential.

### 📊 Reports

| Report | What it answers |
|---|---|
| Web Sales | By day, item or category, in the company currency |
| Top Products | What sells most |
| Abandoned Carts | Value, items and how many can be contacted |
| Conversion Funnel | Carts created, with items, checked out, paid |
| Coupon Usage | Which codes were used and how often |

### 🔐 Roles, personal data and assistant

- **Personal-data requests** export and erase a shopper's cart contact and address details, stock-alert emails and the name on reviews (the review stays, unsigned). Payment requests are kept as payment records, and the request says why.
- **Assistant.** It knows cart, payment and refund states and suggests "Which carts were abandoned today?" and "Are there payments that need attention?".
- **Rate limits.** The server limits requests per IP on the public routes.

## 🧭 How it works

### Checkout and payment

```mermaid
sequenceDiagram
    autonumber
    actor Shopper
    participant Store as Storefront
    participant Engine as Selling engine
    participant ERP as ERP sales order
    participant Gateway as Gateway plugin
    Shopper->>Store: add items, apply a code
    Store->>Engine: price and tax the cart
    Engine-->>Store: totals
    Shopper->>Store: checkout (idempotent)
    Store->>ERP: customer, address, submitted sales order
    Store-->>Shopper: payment request
    Gateway->>Store: mark paid
    Store->>ERP: payment entry, applied at invoicing
```

The cart is priced by the selling engine each time it changes. Checkout creates the order and a payment request in one transaction, and a gateway plugin confirms payment, which ERP books as an advance applied when the order is invoiced.

### From order to refund

```mermaid
flowchart LR
    O["Web order"] --> P["Paid"]
    P --> S["Shipped (in parts, tracked)"]
    S --> I["Invoiced"]
    I -->|"shopper asks to return"| RQ["Return request"]
    RQ -->|"desk approves, goods received"| RF["Refund approved"]
    RF --> GW["Gateway sends money back"]
    GW --> BK["Credit note, restock, bank entry"]
    O -->|"cancel before shipping"| RF
    classDef app fill:#be185d,color:#fff,stroke:#be185d
    class O,RQ,RF app
```

Refunds follow one path whether the goods come back or the order is cancelled. A person approves, the gateway the order was paid with carries it out, then the books follow.

## 🧱 Data model

| Entity | What it is | Key fields or states |
|---|---|---|
| StoreSettings | The one store record | Company, currency, price list, warehouses, payment methods, after-sales windows, languages |
| ItemCategory | A category in a tree | Name, slug, published, SEO |
| WebsiteItem | What the store shows of an ERP item | Slug, published, ranking, variant of, options, images, categories, related, SEO |
| ItemReview | A shopper's review | Rating, verified purchase, Pending or approved |
| StockAlert | A back-in-stock request | Email, told once |
| ShoppingCart | A sales-shaped cart | Status (Open, Abandoned, Ordered, Expired), lines, taxes, delivery place, promotions, totals |
| CouponCode | A code that unlocks a pricing rule | Validity, minimum order, total and per-customer limits, used count |
| ShippingRule | A delivery method | Basis (Fixed, Order Amount, Total Weight), bands, countries, regions, postal ranges, free over |
| PaymentRequest | A request to pay an order or invoice | Status (Initiated, Paid, Failed, Cancelled, Expired), gateway, amount, link, reference |
| PaymentException | Money taken that could not be booked | Open until refunded or resolved |
| OrderRefund | Money sent back | Kind (Return, Cancellation, Payment, Adjustment), status (Requested, Approved, Refunded, Refund Failed, Declined), books posted |
| ReturnRequest, ReturnReason | A shopper's request and why goods come back | Requested, approved, declined, received |
| StorefrontWords | A language's storefront words | One JSON record per language |

Lines and child tables (cart items, website item images, categories, related items and options, refund and return lines, shipping bands, countries, regions and postal ranges, store warehouses, countries and payment methods) hold the rest. The Store Subscriptions connector adds a plan to a website item and a subscription to a web order.

### 👥 Roles

| Role | What it can do |
|---|---|
| Website Manager | Runs the store: settings, catalog, reviews, shipping, coupons, promotions, payments, refunds, returns, storefront words, and ships and invoices web orders |
| Sales Manager | Reads carts, payments, shipping and coupons; works refunds and returns |
| Sales User | Reads the store's records; can raise a refund |
| Accounts Manager | Works payment requests and refunds |
| Accounts User | Works payment requests; reads refunds |

### ⏱️ Upkeep

Every minute, one replica at a time: expire overdue payment requests (cancelling unpaid orders as configured and releasing their stock), mark idle carts Abandoned and old carts Expired, send cart reminders, and read carrier shipments to tell shoppers about delivery.

## 🔌 Integrations

| With | How | Needs |
|---|---|---|
| ERP | Items, customers, price lists, stock, sales orders, invoices, promotions, gift cards and store credit are ERP's | ERP 0.24 or later (ERP 0.25 for the carrier tests) |
| Stripe, PayPal, Razorpay | Built as WebAssembly plugins in `gateways/`; webhooks signed and checked | Your own provider account, keys and webhook |
| Finance | The Store Subscriptions connector sells plans | Finance installed |
| Stock Operations | Generate Variants | Installed |
| Bookings and other apps | Ask for a payment or a part refund through `request_invoice_payment` and `refund_payment` | The other app installed |
| Retail | Returns at a store against Commerce's invoice; the same stock and customers | Retail installed |
| Mail, Campaigns | Shopper mails are message templates; events feed journeys | Mail installed |
| Carriers | ERP's carrier shipments are read to tell the shopper | A carrier connection in ERP |
| Outside tax, shipping and marketplace services | Not built | None |
| REST, MCP, webhooks | Storefront API (public and signed-in) and every record | API keys |

## 📊 How it compares

Compared with Shopify, BigCommerce, WooCommerce, Adobe Commerce (Magento) and, as an open-source option that also includes accounting, Odoo eCommerce.

| Capability | Ferrum Commerce | Shopify | BigCommerce | WooCommerce | Adobe Commerce | Odoo eCommerce |
|---|:-:|:-:|:-:|:-:|:-:|:-:|
| Catalog with categories, images and SEO fields | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| Variants with a generator and picker | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| Product attributes, filters and facets on listings | ❌ | ✅ | ✅ | ✅ | ✅ | ✅ |
| Bundles and kits | 🟡 [1] | ✅ | 🟡 | 🟡 | ✅ | ✅ |
| Digital goods and downloads | ❌ | 🟡 | ✅ | ✅ | ✅ | 🟡 |
| Pre-orders and back-orders | ❌ | 🟡 | ✅ | ✅ | ✅ | ✅ |
| Guest and signed-in checkout | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| Customer account pages on the storefront | 🟡 [2] | ✅ | ✅ | ✅ | ✅ | ✅ |
| Promotions: percent, amount, buy and get, free shipping, stacking | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| Coupon codes with usage limits | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| Gift cards and store credit | ✅ | ✅ | ✅ | 🟡 | 🟡 | ✅ |
| Shipping zones by country, region and postcode | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| Live carrier rates and shipping labels | ❌ | ✅ | ✅ | 🟡 | ✅ | ✅ |
| Partial shipments from several warehouses | ✅ | ✅ | ✅ | 🟡 | ✅ | ✅ |
| Tax by delivery place | 🟡 [3] | ✅ | ✅ | ✅ | ✅ | ✅ |
| Card and wallet payments through major gateways | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| A wide catalogue of payment providers | ❌ | ✅ | ✅ | ✅ | ✅ | ✅ |
| Saved cards, Apple Pay and Google Pay on the storefront | ❌ | ✅ | ✅ | 🟡 | 🟡 | 🟡 |
| Edit an order after it is placed | ✅ | ✅ | ✅ | ✅ | 🟡 | ✅ |
| Shopper self-service cancel and return | ✅ | ✅ | 🟡 | 🟡 | ✅ | 🟡 |
| Return requests with approval and refund | ✅ | ✅ | ✅ | 🟡 | ✅ | ✅ |
| Refunds book credit notes, stock and bank entries | ✅ | ❌ | ❌ | ❌ | 🟡 | ✅ |
| Abandoned-cart reminders | ✅ | ✅ | ✅ | 🟡 | 🟡 | ✅ |
| Back-in-stock alerts | ✅ | 🟡 | 🟡 | 🟡 | ✅ | 🟡 |
| Reviews with moderation and verified purchase | 🟡 [4] | 🟡 | ✅ | ✅ | ✅ | ✅ |
| Themes and a page builder | ❌ | ✅ | ✅ | ✅ | ✅ | ✅ |
| Content pages and a blog | ❌ [5] | ✅ | ✅ | ✅ | ✅ | ✅ |
| Storefront in several languages | 🟡 [6] | ✅ | ✅ | 🟡 | ✅ | ✅ |
| Several currencies shown to shoppers | ❌ | ✅ | ✅ | 🟡 | ✅ | ✅ |
| Several storefronts from one catalog | ❌ | 🟡 | ✅ | 🟡 | ✅ | ✅ |
| B2B: company accounts, net terms, quotes | ❌ | 🟡 | 🟡 | 🟡 | ✅ | 🟡 |
| Subscriptions | 🟡 [7] | 🟡 | 🟡 | 🟡 | 🟡 | 🟡 |
| Marketplaces and social-channel selling | ❌ | ✅ | ✅ | 🟡 | 🟡 | 🟡 |
| Product feeds for Google and Meta | ❌ | ✅ | ✅ | ✅ | 🟡 | 🟡 |
| Fraud checks | ❌ | ✅ | ✅ | 🟡 | 🟡 | ❌ |
| Documented storefront API | 🟡 [8] | ✅ | ✅ | ✅ | ✅ | 🟡 |
| Orders, stock, invoices and books in one system | ✅ | ❌ | ❌ | ❌ | ❌ | ✅ |
| Self-hosted, open source | ✅ | ❌ | ❌ | ✅ | 🟡 | 🟡 |
| No platform revenue share or transaction fee from the vendor | ✅ | ❌ | ✅ | ✅ | ✅ | ✅ |

Notes. 🟡 for a leader means an app or extension, a higher plan, or only part of the feature. Adobe Commerce 🟡 for open source means its Open Source edition is free but the commercial edition is not; Odoo 🟡 means the Community edition is open source and Enterprise is not.

1. Promotions can bundle (buy and get, bundle prices) and ERP's Stock Operations has product bundles, but Commerce does not yet sell a bundle as one product stocked as parts.
2. The signed-in API has a customer's orders, invoices, reorder and subscriptions; no storefront page signs in yet. Order tracking works by link.
3. The cart carries the delivery country and region and ERP's country rules read them; no automatic tax-rate service, and no tax IDs or exemptions at checkout.
4. Reviews are taken by the API and moderated; the storefront pages show them but do not yet have a review form.
5. Ferrum's Website app serves pages and a blog on its own host; the store's own host serves only the shop.
6. Words are translatable per language and right-to-left is handled; English is the only language that ships, product text is not translated, and the header, listing and product page words the server writes are still English.
7. A Finance plan can be sold as an item with renewals paid online; no saved cards for automatic renewal and no plan change from the portal.
8. Documented REST, public and signed-in; no GraphQL and no API keys for the storefront routes.

### 🏆 Where Ferrum Commerce stands out

- **The shop and the books are one system.** Orders are ERP sales orders; stock reserved by orders is the same stock the warehouse sees; refunds post credit notes and bank entries.
- **A price that never differs.** Cart, order and invoice come from the same engine, including promotions, tax and rounding.
- **Real after-sales.** Order edits with a priced preview, shopper self-service cancel and return with a signed link, windows, restocking fees and store credit, goods-received step, and parts shipped from several warehouses.
- **Safe payments.** Plugins hold no keys, requests are made once however often events arrive, a lost webhook is caught by a five-minute reconcile, and unbookable money is a visible exception.
- **Open and self-hosted.** No revenue share, no per-order fee, and every record open to REST and MCP.

### 🚧 Where the leaders are still ahead

- **The storefront's look and reach:** themes, a page builder, content pages, menus, customer account pages, express wallets, saved cards, several languages and currencies, and several storefronts.
- **Catalog depth:** attributes and facets, bundles, digital goods, pre-orders, product feeds, and a media pipeline.
- **Logistics:** live carrier rates, labels, tracking sync and customs documents.
- **Payments breadth:** many more providers, authorise-then-capture, chargebacks, fraud scoring and gateway-fee booking.
- **Selling channels:** marketplaces, social shops, loyalty and B2B features.

> [!NOTE]
> Leader columns are from public product information as of November 2026 and may change. Plans and add-ons differ; check each vendor.

## 📍 Roadmap

### ✅ Recently shipped

- [x] **0.10.4** — Store Settings pickers offer the store's company's warehouses, taxes and accounts.
- [x] **0.10.3** — Choices limited to the company; Store Settings fills the company in; the refusal names the records.
- [x] **0.10.2** — Reports take the store's company; a first-run state says the store is not set up yet.
- [x] **0.10.1** — Screens for order change previews, cancel and refund, send a mail again, and paying the rest.
- [x] **0.10.0** — Pay the rest after a card paid part, a preview of an order change, the shopper's link pages with the store's terms, send mail again, and storefront words per language.
- [x] **0.9.0** — Order changes, shopper cancel and return, tax and shipping by place, editable shopper mails, variants generated through ERP.
- [x] **0.8.0** — Promotions through ERP, gift cards and store credit, and orders shipped in parts from several warehouses.

### 🔜 Next up

**Storefront**
- [ ] Brand settings: logo, colours, fonts, header, footer and an announcement bar.
- [ ] A designed home page with banners and featured collections, and navigation menus.
- [ ] Customer account pages: sign in, order history, saved addresses, reorder, subscriptions.
- [ ] A review form on the product page.
- [ ] Product attributes and filters on listings.
- [ ] Content pages and a blog on the store's host.
- [ ] Redirects when a slug changes.
- [ ] Analytics tags and a cookie-consent banner.

**Checkout**
- [ ] Extra checkout fields (company name, tax ID, PO number, gift message) and a minimum order value.
- [ ] Address validation and postcode formats per country.
- [ ] Payment method rules by country, amount and customer group.
- [ ] Express wallets and saved cards.
- [ ] Tax IDs, exemptions and reverse charge.

**Operations**
- [ ] Live carrier rates, labels and tracking sync (Shippo, EasyPost, Shiprocket-style carriers).
- [ ] Staff-made orders with a payment link.
- [ ] Goodwill refunds without goods, and exchanges.
- [ ] Authorise at order and capture at shipment.

### 🔭 Later

- [ ] More payment providers (Adyen, Mollie, Cashfree, PayU, Klarna and local methods).
- [ ] Chargebacks and disputes; gateway fees and payout reconciliation booked.
- [ ] Fraud checks and holds.
- [ ] Bundles sold as one product; digital goods; pre-orders and back-orders.
- [ ] Loyalty points and referrals; wishlists.
- [ ] Company accounts, net terms and credit limits at checkout, quotes, quick order.
- [ ] Several currencies shown to shoppers; several storefronts from one catalog.
- [ ] Product feeds for Google Merchant Center and Meta; marketplaces (Amazon, Flipkart and similar) and social shops.
- [ ] SMS and WhatsApp order updates.
- [ ] Rule-based collections, best-seller sorting, and typo-tolerant search.
- [ ] Product media upload with resizing and video.
- [ ] Publish and unpublish on a date.
- [ ] Traffic sources and conversion by channel.
- [ ] Import from Shopify, WooCommerce and Magento (customers, orders, redirects).
- [ ] A GraphQL API and API keys for headless use.
- [ ] A setup wizard and launch checklist.
- [ ] An installable storefront (PWA).

## 🚀 Install

Commerce is compiled into the Ferrum server image and installed from **Apps** (`/store/`) by a workspace administrator; installing it installs ERP (`>=0.24`). Self-hosters who build their own image enable the `commerce` feature of the server. It needs Ferrum `>=0.3.6, <0.4`.

After installing:

1. Open **Store Settings** and fill in the company, currency, price list, tax template, warehouses (and the fulfilment warehouse), the shipping item, allowed countries, and payment methods. The pickers offer only the chosen company's records.
2. Give people the roles **Website Manager** (and **Sales Manager**, **Sales User**, **Accounts Manager** or **Accounts User** as they fit).
3. Add **Shipping Rules**, **Item Categories** and **Website Items** (or import them as CSV); use **Generate Variants** for sizes and colours.
4. Install a payment gateway plugin: build `gateways/` for `wasm32-unknown-unknown`, install the `.wasm` with its `manifest.json` as an administrator, point the provider's webhook at `https://<your host>/api/public/<workspace>/<gateway>/webhook`, and add a payment method for it in Store Settings. Cash on Delivery and bank transfer need no plugin.
5. In **Domains**, point a host at the store (target `site:commerce`) so the storefront is served at its root.
6. Review the **After Sales** tab (cancellation and return windows, restocking fee, where refunds go) and edit the shopper mail templates.
7. Optional: install the Store Subscriptions connector where Finance is installed.

## 🔧 Develop

Clone it next to the platform:

```text
work/
├── ferrum/            https://github.com/CBFerrum/Ferrum (with Ferrum-Frontend cloned into ferrum/web)
└── ferrum-app-commerce/
```

Point the platform dependencies at your checkout (this file is git-ignored):

```toml
# .cargo/config.toml
[patch."https://github.com/CBFerrum/Ferrum"]
ferrum-apps = { path = "../ferrum/crates/apps" }
ferrum-domain = { path = "../ferrum/crates/domain" }
ferrum-kernel = { path = "../ferrum/crates/kernel" }
ferrum-meta = { path = "../ferrum/crates/meta" }
```

Commerce also builds on [ferrum-app-erp](https://github.com/CBFerrum/ferrum-app-erp), which Cargo takes from GitHub. To work on it alongside, clone it into the same folder and add:

```toml
[patch."https://github.com/CBFerrum/ferrum-app-erp"]
ferrum-app-erp = { path = "../ferrum-app-erp" }
```

The tests need PostgreSQL with an ordinary (non-superuser) role, so row-level security applies:

```bash
createuser ferrum --pwprompt && createdb -O ferrum ferrum_commerce_test
FERRUM_COMMERCE_TEST_DATABASE_URL=postgres://ferrum:ferrum@localhost:5432/ferrum_commerce_test cargo test
```

The gateway plugins are a separate workspace in `gateways/`:

```bash
cd gateways
rustup target add wasm32-unknown-unknown
cargo build --release --target wasm32-unknown-unknown
cargo test
```

Gateway tests run against scripted providers; with test-mode keys in the environment (`STRIPE_TEST_SECRET_KEY`, `RAZORPAY_TEST_KEY_ID` and `RAZORPAY_TEST_KEY_SECRET`, `PAYPAL_SANDBOX_CLIENT_ID` and `PAYPAL_SANDBOX_CLIENT_SECRET`) they also talk to the provider's test mode, and without them those tests say they were skipped. Live keys are refused.

The admin interface lives in `ui/` and builds with Ferrum-Frontend's packages (checked out at `../ferrum/web`); the storefront's script and stylesheet are in `assets/`:

```bash
cd ui
npm run check
npm run typecheck
npm test
npm run build      # ui/dist
```

CI (`.github/workflows/ci.yml`) runs the Rust checks on PostgreSQL, the gateway plugins' tests, and the interface checks on Node 22. The storefront API and the gateway plugin contract are described in [packages/commerce/README.md](packages/commerce/README.md).

## 📂 Project layout

```text
app.yaml                   the manifest: id, version, requirements, what it adds to Ferrum
packages/commerce/         entities (cart, catalog, coupons, payments, refunds, shipping, store, words),
                           roles, messages, SQL, and the storefront API notes
packages/commerce_subscriptions/  the Finance connector's package
connectors/subscriptions/  the Store Subscriptions connector manifest
gateways/                  Stripe, PayPal and Razorpay plugins (WebAssembly) and their shared kit
src/                       Rust: cart, checkout, payment, refund, shipping, catalog, storefront pages, upkeep
assets/                    the storefront's script, stylesheet and words
ui/                        the admin interface (React and Vite), built into ui/dist
tests/                     integration tests against a real PostgreSQL
```

## 📜 Changelog

Every version is in [CHANGELOG.md](CHANGELOG.md). The latest is **0.10.4**.

## 🤝 Contributing

Issues and pull requests are welcome at [github.com/CBFerrum/ferrum-app-commerce](https://github.com/CBFerrum/ferrum-app-commerce/issues). Run the checks in the Develop section before you open one. Rebuild `ui/dist/` if you change the admin screens.

## 📄 License

[AGPL-3.0-only](LICENSE), like the platform. The gateway plugins are under AGPL-3.0-or-later.

<div align="center">

<sub>Part of <a href="https://github.com/CBFerrum/Ferrum">Ferrum</a>, the open business platform · Made with 🦀 Rust and 🐘 PostgreSQL</sub>

</div>
