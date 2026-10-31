# Changelog

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
