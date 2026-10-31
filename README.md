<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/assets/banner-dark.svg">
  <img alt="Ferrum Commerce" src="docs/assets/banner-light.svg" width="100%">
</picture>

[![CI](https://github.com/CBFerrum/ferrum-app-commerce/actions/workflows/ci.yml/badge.svg)](https://github.com/CBFerrum/ferrum-app-commerce/actions/workflows/ci.yml) ![Version](https://img.shields.io/badge/version-0.3.2-be185d?style=flat-square) ![Ferrum](https://img.shields.io/badge/Ferrum-%3E%3D0.3.1%2C%20%3C0.4-2f5bea?style=flat-square) ![License](https://img.shields.io/badge/license-AGPL--3.0-3b82f6?style=flat-square) ![Rust](https://img.shields.io/badge/Rust-native%20app-b7410e?style=flat-square&logo=rust&logoColor=white) ![Interface](https://img.shields.io/badge/interface-React%20%2B%20Vite-0ea5e9?style=flat-square&logo=react&logoColor=white) ![MCP](https://img.shields.io/badge/AI%20agents-MCP-8b5cf6?style=flat-square)

**A native app for [Ferrum](https://github.com/CBFerrum/Ferrum), the open-source, AI-first business platform.**

[Overview](#overview) · [Highlights](#highlights) · [Screenshots](#screenshots) · [How it works](#how-it-works) · [Data model](#data-model) · [Install](#install) · [Develop](#develop) · [Changelog](#changelog)

</div>

---

## Overview

Ferrum Commerce sells from the catalog, prices, taxes and stock you already keep in Ferrum.
Carts are priced and taxed by the selling engine on every change; checkout creates the customer,
addresses, a submitted sales order and a payment request in one idempotent transaction; and gateway
plugins mark payment requests paid, which books an advance applied when the order is invoiced.


| At a glance | |
|---|---|
| 📦 Version | `0.3.2` |
| 🧱 Platform | Ferrum `>=0.3.1, <0.4` |
| 🔗 Requires | erp `>=0.1` |
| 🖥️ Interface | its own, at `/commerce/` |
| 🗃️ Records | 17 entities in 1 package |
| 👥 Roles | 5: Website Manager, Sales User, Sales Manager, Accounts Manager, Accounts User |
| 📊 Reports | 5 |
| 🧪 Tests | 21 Rust tests, 1 interface test file |


## Highlights

<table>
<tr>
<td width="50%" valign="top">

### 🛍️ Catalog
Store settings, a category tree and website items with images, SEO fields and reviews.

</td>
<td width="50%" valign="top">

### 🛒 Live carts
Priced and taxed on every change; coupons with usage limits; shipping rules by amount, weight and country.

</td>
</tr>
<tr>
<td width="50%" valign="top">

### ✅ One-step checkout
Guest and portal checkout: customer, contact, addresses, sales order and payment request at once.

</td>
<td width="50%" valign="top">

### 💳 Gateway plugins
Payment requests that plugins mark paid; unpaid orders expire and release their stock.

</td>
</tr>
<tr>
<td width="50%" valign="top">

### 📦 Stock that counts orders
Open orders reserve stock, locked per item at checkout.

</td>
<td width="50%" valign="top">

### 📊 Store reports
Web sales, abandoned carts, the conversion funnel, top products and coupon usage.

</td>
</tr>
</table>

## Screenshots

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/assets/commerce-dark.webp">
  <img alt="The store's catalog: published website items with their rank and descriptions." src="docs/assets/commerce-light.webp" width="100%">
</picture>

<p align="center"><sub>The store's catalog: published website items with their rank and descriptions.</sub></p>

> [!TIP]
> The screenshots follow your GitHub theme: switch between light and dark to see both.

## How it works

### Checkout

```mermaid
sequenceDiagram
    autonumber
    actor Shopper
    participant Store as Storefront API
    participant Engine as Selling engine
    participant ERP as Sales order (ERP)
    participant Gateway as Gateway plugin
    Shopper->>Store: add items, apply a coupon
    Store->>Engine: price and tax the cart
    Engine-->>Store: totals
    Shopper->>Store: checkout (idempotent)
    Store->>ERP: customer, addresses, submitted sales order
    Store-->>Shopper: payment request
    Gateway->>Store: mark_paid
    Store->>ERP: advance payment, applied at invoicing
```

### Inside Ferrum

```mermaid
flowchart LR
    subgraph Browser
        UI["Its interface<br/><small>/commerce/</small>"]
        BAR["Ferrum bar<br/><small>launcher · search · bell</small>"]
    end
    subgraph Server["Ferrum server"]
        API["REST · MCP · live"]
        K["Kernel<br/><small>permissions · row-level security · audit</small>"]
        APP["Commerce<br/><small>17 entities · 5 roles</small>"]
        OB[("Outbox")]
    end
    DB[("PostgreSQL")]
    UI --> API --> K --> DB
    BAR --> API
    K <--> APP
    K --> OB --> EV["Notifications · webhooks<br/>automations"]
    APP -. requires .-> D0["erp >=0.1"]
    classDef app fill:#be185d,color:#fff,stroke:#be185d
    class APP app
```

Everything Commerce adds is ordinary Ferrum metadata and Rust: its records get lists, forms,
search, permissions, an audit trail, exports and the REST and MCP APIs for free, so AI agents work
with them under the same rules as people.

### What it adds to Ferrum

| Where | What |
|---|---|
| 🚀 Launcher | **Store** at `/commerce/` |
| ➕ Create menu | Store item |
| ➕ Create menu | Coupon |

## Data model

The main records and how they connect (17 entities in all; `packages/` has every field).

```mermaid
erDiagram
    WebsiteItem {
        string title PK
    }
    ItemCategory {
        string category_name PK
    }
    ShoppingCart {
        string status
        string email PK
        decimal grand_total
    }
    ItemReview {
        int rating
        string title PK
        string reviewer_name
    }
    PaymentRequest {
        string order_number PK
        string gateway
        decimal amount
    }
    ShippingRule {
        string label PK
        string basis
        decimal amount
    }
    Item ||--o{ WebsiteItem : "item"
    Customer ||--o{ ShoppingCart : "customer"
    ShoppingCart ||--|{ ShoppingCartItem : "items"
    ShoppingCart ||--|{ TaxRow : "taxes"
    ShippingRule ||--o{ ShoppingCart : "shipping_rule"
    SalesOrder ||--o{ ShoppingCart : "sales_order"
    PaymentRequest ||--o{ ShoppingCart : "payment_request"
    WebsiteItem ||--o{ ItemReview : "website_item"
    Customer ||--o{ ItemReview : "customer"
    SalesOrder ||--o{ PaymentRequest : "sales_order"
    Customer ||--o{ PaymentRequest : "customer"
```

### Roles

| Role | What it is for | Entities |
|---|---|---|
| **Website Manager** | Runs the online store - settings, catalog, categories, review moderation, shipping, coupons and payment requests. | 22 |
| **Sales User** |  | 7 |
| **Sales Manager** |  | 7 |
| **Accounts Manager** |  | 2 |
| **Accounts User** |  | 1 |

<details>
<summary><b>Reports (5)</b></summary>

- Web Sales
- Top Products
- Abandoned Carts
- Conversion Funnel
- Coupon Usage

</details>

## Install

Workspace administrators install it from **Apps** (`/store/`) in Ferrum; it ships in the
official Ferrum server image. It needs **erp** `>=0.1` installed first. Self-hosters building their own image enable the
`commerce` feature of the server. Everything it adds is also available to AI agents over
Ferrum's MCP server, within each person's permissions.

## Develop

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
ferrum-calc = { path = "../ferrum/crates/calc" }
ferrum-domain = { path = "../ferrum/crates/domain" }
ferrum-kernel = { path = "../ferrum/crates/kernel" }
ferrum-meta = { path = "../ferrum/crates/meta" }
```

The tests need PostgreSQL with an ordinary (non-superuser) role, so row-level security
applies:

```bash
createuser ferrum --pwprompt && createdb -O ferrum ferrum_commerce_test
cargo test
```

`FERRUM_COMMERCE_TEST_DATABASE_URL` overrides the database URL.


It also builds on [ferrum-app-erp](https://github.com/CBFerrum/ferrum-app-erp), and its tests on [ferrum-app-crm](https://github.com/CBFerrum/ferrum-app-crm), which Cargo takes from GitHub.
To work on them alongside this app, clone them into the same folder and add to the same file:

```toml
[patch."https://github.com/CBFerrum/ferrum-app-crm"]
ferrum-app-crm = { path = "../ferrum-app-crm" }
[patch."https://github.com/CBFerrum/ferrum-app-erp"]
ferrum-app-erp = { path = "../ferrum-app-erp" }
```


The interface lives in `ui/` and builds with Ferrum-Frontend's packages:

```bash
cd ui
npm run build      # ui/dist, checked against the size budget
npm test
```

With a development server started with `FERRUM_DEV_UI=1`, `ferrum-app dev --ui` serves the pages
from Vite behind the real bar and session, with hot reload.


### Project layout

```text
app.yaml      the manifest: id, version, requirements, what it adds to Ferrum
packages/     metadata: entities, roles, reports, workflows, print formats
src/          the app's logic in Rust: hooks, actions, endpoints, upkeep jobs
ui/           its own interface (React + Vite), built into ui/dist
tests/        integration tests against a real PostgreSQL
CHANGELOG.md  what changed in each version
```

## Changelog

| Version | Highlights |
|---|---|
| **0.3.2** | The interface starts faster: its first download holds the frame and the app's own pages, and lists, forms, reports and other apps' screens load … |
| **0.3.1** | Record cards (the bar's previews on hover, Ferrum apps-ui.md U5) stay off the web orders to deliver and the payments that did not go through … |
| **0.3.0** | The storefront on a host of its own: on a workspace host whose target is `site:commerce`, the shop's pages are at its root, rendered on the server … |
| **0.2.0** | The store's admin, an interface of its own at /commerce/, in `ui/` (built with Ferrum-Frontend's packages, `dist` committed and embedded as `UI`) … |

Every version is in [CHANGELOG.md](CHANGELOG.md).

## License

[AGPL-3.0-only](LICENSE), like the platform. Part of [Ferrum](https://github.com/CBFerrum/Ferrum) ·
[Report an issue](https://github.com/CBFerrum/ferrum-app-commerce/issues)
