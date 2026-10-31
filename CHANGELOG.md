# Changelog

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
