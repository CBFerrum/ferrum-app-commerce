# Changelog

## 0.7.10

Rebuilt with Ferrum-Frontend a64416a: a new record's box whose default is 1 shows ticked, as it is saved. An action's dialog says every refusal, the errors on what the action made (another record's rows) among them, and an action with parameters and a confirmation asks once.

## 0.7.9

Rebuilt with Ferrum-Frontend a76295e: its pages show times on a person's own clock when they chose one on their Account page, while today and due dates stay on the workspace's day. Its pages count today on the workspace's day, not UTC's, so the day's figures are right in the evening west of Greenwich and the early morning east of it.

## 0.7.8

Its storefront says it serves pages at the root of a `site:commerce` host, so Ferrum's Domains offers the storefront, and only apps that serve a site (Ferrum PR 85). A shipped order reads "Shipped" to the shopper, not "Delivered": nothing tells the store a parcel arrived, and an order shipped and invoiced at once was called delivered as it left. The shopper is e-mailed through the server's mailer: the order with its link and how to pay, that it has shipped when the desk says to tell them (carrier, parcel and link), and money sent back when the refund says so. Before, only events went out, and nothing in a workspace sent them (both found walking an order through the storefront).

## 0.7.7

An order's story gives each step's full time on the workspace's clock. Rebuilt with Ferrum-Frontend 24c3951, so its records' pages show times on the workspace's clock too, a draft numbered on submit is headed as a draft, and a record of an app that is not installed says so.

## 0.7.6

Rebuilt with Ferrum-Frontend bb60115: a record page reads an empty number or check as the server does, so an action whose condition needs one shows, and a workspace's settings open as their one record.

## 0.7.5

A payment request's page asks for what its actions read: Mark Paid needs the amount and currency received and never asked, so from the request's page it always failed (the order page sends them); it now asks, filled with the request's own amount and currency, and takes the currency as the page names it. Mark Failed asks for its reason and reference, and Set Gateway Details for the payment link and reference.

## 0.7.4

Rebuilt with Ferrum-Frontend 00dea07: in a workflow state that names what may change, the other fields show read-only, and the fields the state needs show required.

## 0.7.3

Personal-data requests find shopping cart records by the person's phone number as well as their email (Ferrum's FERRUM-PRIVACY-002).

## 0.7.2

Rebuilt with Ferrum-Frontend daeadda: after an action runs, a record page shows what the action said it did rather than only that it is done.

## 0.7.1

Rebuilt with Ferrum-Frontend 80ff179: a field's label names its control for assistive technology, and a record whose title is a link shows that record's title, not its id.

## 0.7.0

For apps that take payments through the store (Bookings' deposits first):

- A payment request can ask for part of an invoice, a deposit, rather than all of it.
- A refund can be of part of a payment, not only of an order (kind Payment). The app that took the
  money works out how much goes back (a booking's cancellation terms), against the credit note it made;
  a person still approves it, the gateway carries it out, and the money out is posted as for an
  order's refund. `commerce.payment_refunded` goes out.
- `ferrum_app_commerce::request_invoice_payment` and `refund_payment` are what other apps call.

## 0.6.0, and Store Subscriptions 0.1.0

Subscriptions sold in the store (FERRUM-COMMERCE-002), in a connector app with Finance, Store
Subscriptions, offered where both are installed. Finance bills them; the store sells them and asks for
each renewal.

- A website item can be sold as a Finance subscription plan. Its web order pays the first period, and
  the subscription it starts is billed by Finance from the second, one subscription per plan line.
  Price the item at the plan's price.
- Each renewal invoice Finance makes gets a payment request through the order's payment method, to
  the order's email: a gateway plugin collects it as it does an order's. Payment requests can now pay
  an invoice as well as an order (Commerce 0.6.0): the payment is allocated to the invoice, and
  `commerce.invoice_paid` goes out.
- Signed-in customers see their subscriptions in the portal (`GET subscriptions`): the plans, the status
  and whether it gives what it sells, the period, and the renewals still to pay with the request that
  pays each. They pay a renewal through any of the store's methods and cancel, the subscription then
  running to the end of the period paid for.
- Cancelling the web order cancels the subscriptions it started.
- Other apps ask whether a customer has an active subscription to a plan with
  `ferrum_app_commerce::entitled`.

Not yet: an email of its own for a renewal (the payment request and the `commerce.invoice_paid` event
are there for a mail or gateway plugin to tell the customer), cards kept on file for charging renewals
without the customer, and changing plans from the portal.

## 0.5.0

- The store's admin has a web-order desk (Orders). It sorts the web orders into what waits for money,
  what is to be picked and sent, and what is sent but not invoiced, longest-waiting first, each with
  the one thing to do next: ship (carrier, tracking number and link, and whether to tell the shopper,
  with the invoice made at the same time), invoice, or mark a bank transfer or cash as paid. A pick list
  turns the orders to send into what to take off the shelves, and prints. Each order has a page of its
  own: items, every payment with its reference and expiry, shipments with tracking, invoices, refunds,
  the customer, where it goes and the story so far. A Website Manager ships and invoices web orders
  without holding the Sales roles (two actions on web orders do exactly that). An order placed before this
  version does not remember how it is paid, so the desk reads that from the order's latest payment request.
- Money that needs a person has one screen (Money): refunds waiting for a decision, and payments a gateway
  took and could not book. A manager approves a refund (the amount, the gateway and a warning that it
  sends money are on the confirmation), declines it with a reason, or says it was refunded another way.
  The tab counts what waits. A gateway still can never approve one.
- Refunds and returns of paid orders. Choose the delivered lines and how many come back, whether each goes
  back on the shelf, and why; or cancel an order that has not shipped. The server works out the amount
  (it makes the credit note and reads what it comes to), holds it to what the payment still has, and shows
  it as you choose. A person approves; the gateway the order was paid with carries it out and reports back;
  then the books follow, all or nothing: a return delivery note, a credit note, the money posted out of the
  bank and matched to what the credit note owes, the order closed when nothing is left to ship (or, for a
  cancellation, the payment reversed and the order cancelled). If the books refuse, the refund stays
  "Refunded", says why, and can be posted again. Cash and bank transfers are refunded by hand and recorded.
  Gateway plugins must learn to carry these out: see "Refunds of booked payments" in the package README.
- Orders can be followed from a link with no sign-in: the order's number and token in the address' fragment
  (no server or log ever sees it) open a page with the steps, the carrier and a link to the parcel, refunds
  on their way, and only the town it goes to. The link comes with the checkout result and the order and
  shipping events; the desk copies it too.
- Store items can have variants (a size, a colour). A variant is an item of its own, with its own price,
  stock and code, that says which item it is a variant of and what sets it apart. The item is listed once,
  "From" its cheapest variant; its page shows the chosen variant's price, stock and picture, and the other
  choices as plain links; orders, stock and reviews follow without change. Search finds each word of a
  query anywhere in an item, its variants and their choices.
- The storefront on a phone: sorting and "in stock only" in the address, an empty category, no search results
  and nothing in stock each say what to do next; the cart has quantity steppers and a placeholder while it
  loads; checkout has two named steps, a folded summary, each refused field marked beside the field, and for
  an online payment waits for the gateway's page; every view has a failure with a way on.
- New events: `commerce.order_shipped` and `commerce.order_refunded`; `commerce.order_placed` carries the
  order's tracking link. New fields on delivery notes (tracking number and link, whether to tell the
  customer) and on web orders (the method that pays them, and "Partly Refunded" and "Refunded" payment states).

## 0.4.0

- A payment that cannot be booked is a Payment Exception a manager sees, not a log line. Someone approves a refund,
  which the gateway carries out once however often it is retried, or records how it was settled.
- Gateways no longer hold their API keys or webhook secrets: the plugin host does.
- Personal data: a person's request now exports their payment requests, and says why they are kept: they are payment records.

## 0.3.2

- The interface starts faster: its first download holds the frame and the app's own pages, and
  lists, forms, reports and other apps' screens load when first opened (Ferrum
  docs/plan/apps-ui.md §12.9). The build keeps it within the size budget.

## 0.3.1

- Record cards (the bar's previews on hover, Ferrum apps-ui.md U5) stay off the web orders to deliver and the payments that did not go through, whose rows
  already show what a card would.

## 0.3.0

- The storefront on a host of its own (Ferrum docs/plan/apps-ui.md §7.3, U7): on a workspace host
  whose target is `site:commerce`, the shop's pages are at its root, rendered on the server so
  search engines and link previews read them: the home and categories with their items a page at
  a time, search, product pages (images, price with or without tax as the store shows it, stock,
  description as plain text, reviews, related items, schema.org Product data, Open Graph), a
  robots.txt and a sitemap.xml. Until the store has its settings the site says it opens soon.
- The cart (quantities, removing, discount codes), checkout (contact, delivery and billing
  addresses, delivery method, payment method, terms, the total the customer saw; one order per
  attempt with an Idempotency-Key) and the order page (what was ordered, what is due, paying an
  open payment request) are `assets/store.js` over the storefront API, which the platform names in
  `x-ferrum-site-api`. Product pages work without scripts; the cart needs them.
- Pages leave no markup from the catalog: names, descriptions and reviews are escaped, and images
  are the workspace's public files or https addresses.
- Requires the platform with `site:{app}` hosts (Ferrum, apps-ui.md U7).

## 0.2.0

- The store's admin, an interface of its own at /commerce/ (Ferrum docs/plan/apps-ui.md U5), in
  `ui/` (built with Ferrum-Frontend's packages, `dist` committed and embedded as `UI`): web sales
  today and over the last 7 and 30 days, web orders still to deliver (opened in ERP, where they
  are delivered and billed), payments that failed or expired, reviews waiting for moderation
  (published or rejected in place) and the catalog's state. Live updates keep it current. The
  store's records are lists and forms in the records frame.
- Launcher entry (Store) for Website and Sales Managers; Store item and Coupon in the Create menu;
  /open of the store's records into its forms.

## 0.1.1

- Requires the ERP app (Ferrum docs/plan/apps-ui.md §13): installing it in a workspace without ERP
  installs ERP whole, with its interface, rather than a few of its packages.
- Declares where it keeps people's personal data, so Administration → Personal data exports and
  erases it for a person (by email).

## 0.1.0

- First release as a standalone Ferrum app (previously part of the Ferrum repository).
