# Ferrum Commerce

A native app for [Ferrum](https://github.com/CBFerrum/Ferrum), the open-source, AI-first ERP.

- Store settings, item categories (tree) and website items with images, SEO fields and reviews
- Carts priced and taxed by the selling engine on every change; coupons with usage limits; shipping rules by amount or weight and country
- Guest and portal checkout in one idempotent transaction: customer, (with the CRM app) contact and addresses, a submitted sales order and a payment request
- Payment requests for gateway plugins: `mark_paid` books an advance payment applied when the order is invoiced; unpaid orders expire and release their stock
- Stock that counts open orders, locked per item at checkout
- Reports: web sales, abandoned carts, conversion funnel, top products, coupon usage
- `packages/commerce/README.md` documents every storefront endpoint and the gateway-plugin contract

## Install

Workspace administrators install it from **Apps** in Ferrum; it ships in the official Ferrum
server image. Self-hosters building their own image enable the `commerce` feature of the server.
Everything it adds is also available to AI agents over Ferrum's MCP server.

## Develop

Clone it next to the platform and point the platform dependencies at your checkout (this file
is git-ignored):

```toml
# .cargo/config.toml
[patch."https://github.com/CBFerrum/Ferrum"]
ferrum-kernel = { path = "../ferrum/crates/kernel" }
ferrum-meta = { path = "../ferrum/crates/meta" }
ferrum-domain = { path = "../ferrum/crates/domain" }
ferrum-calc = { path = "../ferrum/crates/calc" }
```

The tests need PostgreSQL with an ordinary (non-superuser) role — row-level security must apply:

```bash
createuser ferrum --pwprompt && createdb -O ferrum ferrum_commerce_test
```

```bash
cargo test
```

`FERRUM_COMMERCE_TEST_DATABASE_URL` overrides the database URL. The app's metadata lives in `packages/` and is embedded
at build time; `app.yaml` is its manifest (see Ferrum's `docs/plan/apps.md`).

## Interface

The store's admin, Commerce's own interface at `/commerce/` (Ferrum docs/plan/apps-ui.md), is in
`ui/`: web sales, orders to deliver, failed payments, reviews to moderate and the catalog; the
store's records as lists and forms in the records frame. It builds with Ferrum-Frontend's packages
from the sibling checkout (`../../ferrum/web`), and `ui/dist` is committed and embedded in the
crate (`cd ui && npm run build`; CI checks it).

## License

AGPL-3.0-only, like the platform.
