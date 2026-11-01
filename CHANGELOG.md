# Changelog

## 0.5.0

- The store's admin has a web-order desk (Orders). It sorts the web orders into what waits for money,
  what is to be picked and sent, and what is sent but not invoiced, longest-waiting first, each with
  the one thing to do next: ship (carrier, tracking number and link, and whether to tell the shopper,
  with the invoice made at the same time), invoice, or mark a bank transfer or cash as paid. A pick list
  turns the orders to send into what to take off the shelves, and prints. Each order has a page of its
  own: items, every payment with its reference and expiry, shipments with tracking, invoices, refunds,
  the customer, where it goes and the story so far. A Website Manager ships and invoices web orders
  without holding the Sales roles (two actions on web orders do exactly that).
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
