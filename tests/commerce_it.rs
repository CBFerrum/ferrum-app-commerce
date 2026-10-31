//! The online store end to end against a real PostgreSQL (`FERRUM_COMMERCE_TEST_DATABASE_URL`,
//! default `postgres://ferrum:ferrum@localhost:5432/ferrum_commerce_test`; the database is reset).
//!
//! Drives the storefront exactly as a browser would — through the package's `Site` handler,
//! public and signed in — and confirms payments the way a gateway plugin does: as its service
//! user, through the payment request's `mark_paid` action.

use ferrum_kernel::tenant::NewTenant;
use ferrum_kernel::{Ctx, Db, DbConfig, Doc, Error, HookRegistry, Kernel, KernelSettings, ListQuery, Site, SiteRequest};
use ferrum_meta::{Catalog, PermissionDef};
use rust_decimal::Decimal;
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;
use uuid::Uuid;

fn obj(v: Value) -> Doc {
    v.as_object().cloned().unwrap()
}

fn n(s: &str) -> Decimal {
    Decimal::from_str(s).unwrap()
}

fn dec(v: &Value) -> Decimal {
    match v {
        Value::String(s) => Decimal::from_str(s).unwrap_or_else(|_| panic!("not a decimal: {s}")),
        Value::Number(x) => Decimal::from_str(&x.to_string()).unwrap(),
        Value::Null => Decimal::ZERO,
        other => panic!("not a decimal: {other}"),
    }
}

fn uid(doc: &Doc) -> Uuid {
    doc["id"].as_str().unwrap().parse().unwrap()
}

fn sid(doc: &Doc) -> String {
    doc["id"].as_str().unwrap().to_string()
}

async fn fresh_kernel() -> Arc<Kernel> {
    let url = std::env::var("FERRUM_COMMERCE_TEST_DATABASE_URL").unwrap_or_else(|_| "postgres://ferrum:ferrum@localhost:5432/ferrum_commerce_test".into());
    let db = Db::connect(DbConfig { url, max_connections: 12, ..Default::default() }).await.expect("connect");
    sqlx::raw_sql("drop schema if exists public cascade; create schema public;").execute(&db.pool).await.expect("reset");
    let mut packages = ferrum_domain::packages().expect("platform packages");
    packages.extend(ferrum_app_commerce::packages().expect("app packages"));
    packages.extend(ferrum_app_crm::packages().expect("crm packages"));
    let catalog = Catalog::from_packages(packages).expect("catalog");
    let mut hooks = HookRegistry::default();
    ferrum_domain::register(&mut hooks);
    ferrum_app_commerce::register(&mut hooks);
    ferrum_app_crm::register(&mut hooks);
    for source in ferrum_app_commerce::reports() {
        ferrum_domain::reports::add_source(source);
    }
    let k = Kernel::boot(db, catalog, hooks, KernelSettings::default(), None).await.expect("boot");
    ferrum_domain::maintenance(&k).await.expect("maintenance");
    k
}

struct T {
    k: Arc<Kernel>,
    ctx: Ctx,
    site: Arc<dyn Site>,
}

type Res = ferrum_kernel::Result<Value>;

impl T {
    async fn insert(&self, entity: &str, v: Value) -> Doc {
        self.k.insert(&self.ctx, entity, obj(v)).await.unwrap_or_else(|e| panic!("insert {entity}: {e:?}"))
    }
    async fn get(&self, entity: &str, id: Uuid) -> Doc {
        self.k.get(&self.ctx, entity, id, false).await.unwrap_or_else(|e| panic!("get {entity}: {e:?}"))
    }
    async fn submit(&self, entity: &str, id: Uuid) -> Doc {
        self.k.submit(&self.ctx, entity, id, None).await.unwrap_or_else(|e| panic!("submit {entity}: {e:?}"))
    }
    async fn find_one(&self, entity: &str, filters: Value) -> Doc {
        let q = ListQuery { filters: ferrum_kernel::query::parse_filters(&filters).unwrap(), ..Default::default() };
        let page = self.k.list(&self.ctx, entity, &q).await.unwrap();
        assert_eq!(page.rows.len(), 1, "{entity} {filters}");
        page.rows[0].clone()
    }
    async fn count(&self, entity: &str, filters: Value) -> usize {
        let q = ListQuery { filters: ferrum_kernel::query::parse_filters(&filters).unwrap(), limit: Some(500), ..Default::default() };
        self.k.list(&self.ctx, entity, &q).await.unwrap().rows.len()
    }
    async fn sql(&self, sql: &str) {
        let mut tx = self.k.db.tenant_tx(self.ctx.tenant).await.unwrap();
        sqlx::raw_sql(sqlx::AssertSqlSafe(sql.to_string())).execute(&mut *tx).await.unwrap();
        tx.commit().await.unwrap();
    }
    /// One storefront request, as the server would hand it to the package.
    async fn call(&self, user: Option<&Ctx>, method: &str, path: &str, headers: &[(&str, &str)], body: Option<Value>) -> Res {
        let (path, query) = path.split_once('?').unwrap_or((path, ""));
        let query: Map<String, Value> =
            query.split('&').filter(|p| !p.is_empty()).filter_map(|p| p.split_once('=')).map(|(k, v)| (k.into(), json!(v))).collect();
        let req = SiteRequest {
            tenant: self.ctx.tenant,
            user: user.cloned(),
            method: method.into(),
            path: path.into(),
            query,
            headers: headers.iter().map(|(k, v)| (k.to_string(), json!(v))).collect(),
            body: body.map(|b| b.to_string().into_bytes()).unwrap_or_default(),
            client_ip: "203.0.113.7".into(),
        };
        let resp = self.site.handle(&self.k, req).await?;
        assert_eq!(resp.content_type, "application/json");
        assert!(resp.headers.iter().any(|(k, v)| k == "cache-control" && v == "no-store"));
        Ok(serde_json::from_slice(&resp.body).expect("json body"))
    }
    async fn ok(&self, user: Option<&Ctx>, method: &str, path: &str, headers: &[(&str, &str)], body: Option<Value>) -> Value {
        self.call(user, method, path, headers, body).await.unwrap_or_else(|e| panic!("{method} {path}: {e:?}"))
    }
    async fn get_pub(&self, path: &str) -> Value {
        self.ok(None, "GET", path, &[], None).await
    }
    async fn cart(&self, token: &str, method: &str, path: &str, body: Option<Value>) -> Res {
        self.call(None, method, path, &[("x-cart-token", token)], body).await
    }
    async fn cart_ok(&self, token: &str, method: &str, path: &str, body: Option<Value>) -> Value {
        self.cart(token, method, path, body).await.unwrap_or_else(|e| panic!("{method} {path}: {e:?}"))
    }
    async fn new_cart(&self, user: Option<&Ctx>, items: Value) -> (String, Value) {
        let v = self.ok(user, "POST", "/cart", &[], Some(json!({"items": items}))).await;
        (v["token"].as_str().unwrap().to_string(), v["cart"].clone())
    }
    async fn checkout(&self, user: Option<&Ctx>, token: &str, key: &str, body: Value) -> Res {
        self.call(user, "POST", "/checkout", &[("x-cart-token", token), ("idempotency-key", key)], Some(body)).await
    }
    async fn report(&self, name: &str, params: &[(&str, &str)]) -> Value {
        let p: HashMap<String, String> = params.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        ferrum_domain::reports::run(&self.k, &self.ctx, name, &p).await.unwrap_or_else(|e| panic!("report {name}: {e:?}"))
    }
    /// Sellable quantity as the storefront shows it (the store shows quantities in this test).
    async fn available(&self, slug: &str) -> Decimal {
        dec(&self.get_pub(&format!("/items/{slug}")).await["available_qty"])
    }
}

fn field_error(r: &Res, field: &str) -> String {
    match r {
        Err(Error::Validation(f)) => f.iter().find(|x| x.field == field).map(|x| x.message.clone()).unwrap_or_else(|| panic!("no error on {field}: {f:?}")),
        other => panic!("expected a validation error on {field}, got {other:?}"),
    }
}

const GUEST: &str = "asha@example.com";

fn address(country: &str) -> Value {
    json!({"line1": "12 MG Road", "line2": "Flat 4", "city": "Bengaluru", "state": "Karnataka", "postal_code": "560001", "country": country})
}

fn details(email: &str, name: &str, country: &str) -> Value {
    json!({"email": email, "name": name, "phone": "+91 98450 00000", "shipping_address": address(country)})
}

#[test]
fn storefront_end_to_end() {
    // Same runtime shape as the server: unoptimized async hook chains need more than 2 MiB.
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().thread_stack_size(16 << 20).build().unwrap();
    rt.block_on(async { tokio::spawn(scenario()).await.unwrap() });
}

async fn scenario() {
    let k = fresh_kernel().await;
    let tenant = k
        .create_tenant(NewTenant {
            slug: "shop".into(),
            name: "Web Shop".into(),
            admin_email: "admin@shop.test".into(),
            admin_password: "Correct-Horse-9".into(),
            admin_name: None,
            packages: vec!["commerce".into()],
            settings: Some(json!({"fiscal_year_start_month": 4})),
        })
        .await
        .expect("tenant");
    let session = k.login("shop", "admin@shop.test", "Correct-Horse-9").await.expect("login");
    let ctx = k.load_ctx(tenant, session.user, Some(session.token_version), "it".into()).await.expect("ctx");
    let site = k.hooks.site("commerce").cloned().expect("storefront registered");
    let t = T { k: k.clone(), ctx, site };

    // ───────────── no store yet: the storefront is closed ─────────────
    assert!(matches!(t.call(None, "GET", "/catalog", &[], None).await, Err(Error::NotFound(_))));

    // ───────────── company, items, prices, stock, taxes ─────────────
    let inr = t.find_one("Currency", json!({"code": "INR"})).await;
    let company = t.insert("Company", json!({"company_name": "Web Shop Pvt Ltd", "abbr": "WS", "default_currency": inr["id"]})).await;
    let cid = uid(&company);
    let stores = t.find_one("Warehouse", json!({"warehouse_name": "Stores", "company": cid.to_string()})).await;
    let output_tax = t.find_one("Account", json!({"account_name": "Output Tax", "company": cid.to_string()})).await;
    let vat = t
        .insert(
            "TaxTemplate",
            json!({"title": "VAT 18%", "company": cid.to_string(), "kind": "Sales", "is_default": true,
                   "rows": [{"charge_type": "On Net Total", "account": output_tax["id"], "description": "VAT", "rate": "18"}]}),
        )
        .await;
    let products = t.find_one("ItemGroup", json!({"group_name": "Products"})).await;
    let services = t.find_one("ItemGroup", json!({"group_name": "Services"})).await;
    let nos = t.find_one("Uom", json!({"uom_name": "Nos"})).await;
    let item = |code: &str, name: &str, extra: Value| {
        let mut v = json!({"item_code": code, "item_name": name, "item_group": products["id"], "stock_uom": nos["id"]});
        v.as_object_mut().unwrap().extend(extra.as_object().cloned().unwrap());
        v
    };
    let mug = uid(&t.insert("Item", item("MUG", "Blue Mug", json!({}))).await);
    let tee = uid(&t.insert("Item", item("TEE", "Logo Tee", json!({"tax_rates": [{"tax_account": output_tax["id"], "rate": "5"}]}))).await);
    let secret = uid(&t.insert("Item", item("SECRET", "Secret Prototype", json!({}))).await);
    let ship = uid(&t.insert("Item", item("SHIP", "Shipping", json!({"is_stock_item": false, "item_group": services["id"]}))).await);
    let selling = t.find_one("PriceList", json!({"price_list_name": "Standard Selling"})).await;
    for (it, rate) in [(mug, "250"), (tee, "499.5"), (secret, "100")] {
        t.insert("ItemPrice", json!({"item": it.to_string(), "price_list": selling["id"], "rate": rate})).await;
    }
    let opening = t
        .insert(
            "StockEntry",
            json!({"company": cid.to_string(), "purpose": "Opening Stock", "items": [
                {"item": mug.to_string(), "qty": "10", "basic_rate": "90", "target_warehouse": stores["id"]},
                {"item": tee.to_string(), "qty": "3", "basic_rate": "200", "target_warehouse": stores["id"]},
                {"item": secret.to_string(), "qty": "5", "basic_rate": "50", "target_warehouse": stores["id"]}]}),
        )
        .await;
    t.submit("StockEntry", uid(&opening)).await;

    // ───────────── store settings ─────────────
    let individual = t.find_one("CustomerGroup", json!({"group_name": "Individual"})).await;
    let bad =
        t.k.insert(
            &t.ctx,
            "StoreSettings",
            obj(json!({"store_name": "X", "company": cid.to_string(), "currency": inr["id"], "price_list": selling["id"],
            "fulfilment_warehouse": stores["id"], "shipping_item": mug.to_string(), "payment_methods": [{"gateway": "Pay Pal", "label": "x"}]})),
        )
        .await;
    match &bad {
        Err(Error::Validation(f)) => {
            assert!(f.iter().any(|x| x.field == "shipping_item"), "{f:?}");
            assert!(f.iter().any(|x| x.field == "gateway"), "{f:?}");
        }
        other => panic!("bad settings accepted: {other:?}"),
    }
    let settings = t
        .insert(
            "StoreSettings",
            json!({"store_name": "Web Shop", "company": cid.to_string(), "currency": inr["id"], "price_list": selling["id"], "tax_template": sid(&vat),
                   "display_prices_with_tax": true, "warehouses": [{"warehouse": stores["id"]}], "fulfilment_warehouse": stores["id"],
                   "enforce_stock": true, "show_stock_quantity": true, "allow_guest_checkout": true, "default_customer_group": individual["id"],
                   "shipping_item": ship.to_string(), "allowed_countries": [{"country_code": " in ", "country_name": "India"}, {"country_code": "DE", "country_name": "Germany"}],
                   "payment_methods": [{"gateway": "fakepay", "label": "Card", "expiry_minutes": 30},
                                       {"gateway": "Cash on Delivery", "label": "Cash on delivery", "expiry_minutes": 0, "instructions": "Pay the courier."}],
                   "terms_url": "https://shop.example.com/terms", "store_url": "https://shop.example.com"}),
        )
        .await;
    assert_eq!(settings["allowed_countries"][0]["country_code"], json!("IN"), "country codes normalized");

    // ───────────── catalog content ─────────────
    let kitchen = t.insert("ItemCategory", json!({"category_name": "Kitchen & Dining"})).await;
    assert_eq!(kitchen["slug"], json!("kitchen-dining"));
    let mugs_cat = t.insert("ItemCategory", json!({"category_name": "Mugs", "parent_category": sid(&kitchen)})).await;
    let apparel = t.insert("ItemCategory", json!({"category_name": "Apparel"})).await;
    let cycle = t.k.update(&t.ctx, "ItemCategory", uid(&kitchen), obj(json!({"parent_category": sid(&mugs_cat)})), None).await;
    assert!(cycle.is_err(), "category trees cannot loop");
    let mug_wi = t
        .insert(
            "WebsiteItem",
            json!({"item": mug.to_string(), "published": true, "ranking": 10, "weight_kg": "0.4", "short_description": "A sturdy stoneware mug",
                   "long_description": "## Blue Mug\nDishwasher safe.", "images": [{"image": "https://cdn.example.com/mug.jpg", "alt_text": "Blue mug"}],
                   "categories": [{"category": sid(&mugs_cat)}]}),
        )
        .await;
    assert_eq!(mug_wi["title"], json!("Blue Mug"), "title from the item");
    assert_eq!(mug_wi["slug"], json!("blue-mug"));
    t.insert(
        "WebsiteItem",
        json!({"item": tee.to_string(), "title": "Logo Tee", "published": true, "ranking": 5, "weight_kg": "0.2", "max_qty": "5",
                   "categories": [{"category": sid(&apparel)}], "related_items": [{"website_item": sid(&mug_wi)}]}),
    )
    .await;
    t.insert("WebsiteItem", json!({"item": secret.to_string(), "title": "Secret Prototype", "published": false})).await;
    let dup_slug = t.k.insert(&t.ctx, "WebsiteItem", obj(json!({"item": ship.to_string(), "slug": "Blue Mug"}))).await;
    assert!(matches!(dup_slug, Err(Error::Validation(ref f)) if f.iter().any(|x| x.field == "slug")), "{dup_slug:?}");
    let twin = t.k.insert(&t.ctx, "WebsiteItem", obj(json!({"item": mug.to_string(), "slug": "mug-again"}))).await;
    assert!(matches!(twin, Err(Error::Validation(ref f)) if f.iter().any(|x| x.field == "item")), "{twin:?}");

    // ───────────── the storefront's pages on a host of its own (Ferrum apps-ui.md §7.3) ─────────────
    let page = |path: &str, query: &[(&str, &str)], from_server: bool| {
        let mut headers = Map::new();
        if from_server {
            headers.insert("x-ferrum-site-api".into(), json!("/api/site/shop/commerce"));
            headers.insert("x-ferrum-site-origin".into(), json!("https://shop.example.com"));
        }
        let req = SiteRequest {
            tenant,
            user: None,
            method: "GET".into(),
            path: format!("/_site{path}"),
            query: query.iter().map(|(k, v)| (k.to_string(), json!(v))).collect(),
            headers,
            body: vec![],
            client_ip: "203.0.113.7".into(),
        };
        let (site, k) = (t.site.clone(), t.k.clone());
        async move {
            let r = site.handle(&k, req).await?;
            Ok::<_, Error>((r.status, r.content_type, r.headers, String::from_utf8(r.body).unwrap()))
        }
    };
    let (status, ctype, _, home) = page("/", &[], true).await.unwrap();
    assert_eq!((status, ctype.as_str()), (200, "text/html; charset=utf-8"));
    assert!(home.contains(r#"href="/p/blue-mug""#) && home.contains("Logo Tee") && !home.contains("Secret Prototype"), "{home}");
    assert!(home.contains(r#"data-api="/api/site/shop/commerce""#) && home.contains(r#"<link rel="canonical" href="https://shop.example.com/">"#));
    assert!(home.contains(r#"<a href="/c/kitchen-dining">Kitchen &amp; Dining</a>"#), "categories, escaped");
    let (_, _, _, mug_page) = page("/p/blue-mug", &[], true).await.unwrap();
    assert!(mug_page.contains(r#""@type":"Product""#) && mug_page.contains(r#"<img src="https://cdn.example.com/mug.jpg" alt="Blue mug""#), "{mug_page}");
    assert!(mug_page.contains("<p>## Blue Mug<br>Dishwasher safe.</p>"), "the description as plain text: {mug_page}");
    assert!(mug_page.contains(r#"data-item="blue-mug""#), "it can be added to the cart");
    assert_eq!(page("/p/secret-prototype", &[], true).await.unwrap().0, 404, "not published, not there");
    let (_, _, _, cat) = page("/c/kitchen-dining", &[], true).await.unwrap();
    assert!(cat.contains("Blue Mug") && !cat.contains("Logo Tee") && cat.contains(r#"href="/c/mugs""#), "a category with its subcategory's items: {cat}");
    assert_eq!(page("/c/nothing", &[], true).await.unwrap().0, 404);
    let (_, _, _, found) = page("/search", &[("q", "tee")], true).await.unwrap();
    assert!(found.contains("Logo Tee") && !found.contains("Blue Mug") && found.contains(r#"<meta name="robots" content="noindex">"#));
    let (_, _, _, robots) = page("/robots.txt", &[], true).await.unwrap();
    assert!(robots.contains("Sitemap: https://shop.example.com/sitemap.xml") && robots.contains("Disallow: /checkout"));
    let (_, ctype, _, sitemap) = page("/sitemap.xml", &[], true).await.unwrap();
    assert!(ctype.starts_with("application/xml") && sitemap.contains("<loc>https://shop.example.com/p/blue-mug</loc>") && !sitemap.contains("secret"));
    assert!(sitemap.contains("<loc>https://shop.example.com/c/mugs</loc>"));
    let (_, ctype, headers, _) = page("/assets/store.js", &[], true).await.unwrap();
    assert!(ctype.starts_with("text/javascript") && headers.iter().any(|(k, v)| k == "cache-control" && v.contains("immutable")));
    assert_eq!(page("/cart", &[], true).await.unwrap().0, 200);
    assert_eq!(page("/nowhere", &[], true).await.unwrap().0, 404);
    assert!(matches!(page("/", &[], false).await, Err(Error::NotFound(_))), "pages come only through a site host");

    // ───────────── shipping rules ─────────────
    let standard = t
        .insert(
            "ShippingRule",
            json!({"label": "Standard (India)", "basis": "Order Amount", "countries": [{"country_code": "in"}],
                   "bands": [{"from_value": "0", "to_value": "1000", "amount": "80"}, {"from_value": "1000", "to_value": "0", "amount": "0"}]}),
        )
        .await;
    let courier = t
        .insert(
            "ShippingRule",
            json!({"label": "Courier (Germany)", "basis": "Total Weight", "countries": [{"country_code": "DE"}],
                   "bands": [{"from_value": "0", "to_value": "1", "amount": "500"}, {"from_value": "1", "to_value": "5", "amount": "900"}]}),
        )
        .await;
    let overlap = t
        .k
        .insert(&t.ctx, "ShippingRule", obj(json!({"label": "Bad", "basis": "Order Amount", "bands": [{"from_value": "0", "to_value": "500", "amount": "1"}, {"from_value": "400", "to_value": "0", "amount": "2"}]})))
        .await;
    assert!(matches!(overlap, Err(Error::Validation(_))), "overlapping bands refused: {overlap:?}");

    // ───────────── coupons gate their pricing rules ─────────────
    let mug_rule = t.insert("PricingRule", json!({"title": "10% off mugs", "apply_on": "Item", "item": mug.to_string(), "discount_percent": "10"})).await;
    let mug10 = t.insert("CouponCode", json!({"code": " mug10 ", "pricing_rule": sid(&mug_rule), "max_uses": 1, "max_uses_per_customer": 1})).await;
    assert_eq!(mug10["code"], json!("MUG10"));
    let gated = t.get("PricingRule", uid(&mug_rule)).await;
    assert_eq!(gated["condition"], json!(format!("doc.coupon_rule == '{}'", uid(&mug_rule))), "the rule only applies with the coupon");
    let ungate = t.k.update(&t.ctx, "PricingRule", uid(&mug_rule), obj(json!({"condition": null})), None).await;
    assert!(matches!(ungate, Err(Error::Validation(ref f)) if f.iter().any(|x| x.field == "condition")), "the gate stays: {ungate:?}");
    let yesterday = (chrono::Utc::now().date_naive() - chrono::Duration::days(1)).to_string();
    let old_rule = t.insert("PricingRule", json!({"title": "Old promo", "apply_on": "All Items", "discount_percent": "50"})).await;
    t.insert("CouponCode", json!({"code": "OLD50", "pricing_rule": sid(&old_rule), "valid_upto": yesterday})).await;
    let secret_rule =
        t.insert("PricingRule", json!({"title": "Prototype promo", "apply_on": "Item", "item": secret.to_string(), "discount_percent": "5"})).await;
    t.insert("CouponCode", json!({"code": "PROTO5", "pricing_rule": sid(&secret_rule)})).await;
    // A staff order is never discounted by a coupon's rule.
    let sys = Ctx::system(tenant, t.k.registry.schema(tenant).await.unwrap());
    let staff_preview =
        t.k.preview(&sys, "SalesOrder", obj(json!({"company": cid.to_string(), "items": [{"item": mug.to_string(), "qty": "1"}]}))).await.unwrap();
    assert_eq!(dec(&staff_preview["items"][0]["rate"]), n("250"), "no coupon, no discount");

    // ───────────── users: a gateway plugin's service user, portal customers ─────────────
    let gateway_user = |name: &'static str| {
        let k = t.k.clone();
        let admin = t.ctx.clone();
        async move {
            let role = format!("plugin:{name}");
            for entity in ["PaymentRequest"] {
                let p = PermissionDef { entity: entity.into(), read: true, write: true, ..Default::default() };
                k.set_role_permission(&admin, &role, &p).await.unwrap();
            }
            let u = k.create_user(&admin, &format!("{name}@plugins.ferrum.invalid"), &format!("Plugin: {name}"), None, &[role], false).await.unwrap();
            k.load_ctx(tenant, u, None, format!("plugin-{name}")).await.unwrap()
        }
    };
    let fakepay = gateway_user("fakepay").await;
    let otherpay = gateway_user("otherpay").await;
    let portal_user = |email: &'static str| {
        let k = t.k.clone();
        let admin = t.ctx.clone();
        async move {
            let u = k.create_user(&admin, email, "Customer", Some("Correct-Horse-9"), &[], false).await.unwrap();
            k.load_ctx(tenant, u, None, "portal".into()).await.unwrap()
        }
    };
    let rahul = portal_user("rahul@example.com").await;
    let priya_user = portal_user("priya@example.com").await;
    let manager =
        t.k.create_user(&t.ctx, "manager@shop.test", "Store Manager", Some("Correct-Horse-9"), &["Website Manager".to_string()], false).await.unwrap();
    let sales_user = t.k.create_user(&t.ctx, "sales@shop.test", "Sales", Some("Correct-Horse-9"), &["Sales User".to_string()], false).await.unwrap();
    let sales_user = t.k.load_ctx(tenant, sales_user, None, "sales".into()).await.unwrap();

    // ───────────── the public catalog ─────────────
    let store = t.get_pub("/store").await;
    assert_eq!(store["name"], json!("Web Shop"));
    assert_eq!(store["currency"], json!("INR"));
    assert_eq!(store["payment_methods"].as_array().unwrap().len(), 2);
    let cats = t.get_pub("/categories").await;
    assert_eq!(cats["categories"][0]["name"], json!("Apparel"));
    assert_eq!(cats["categories"][1]["slug"], json!("kitchen-dining"));
    assert_eq!(cats["categories"][1]["children"][0]["slug"], json!("mugs"));
    let page = t.get_pub("/catalog").await;
    let items = page["items"].as_array().unwrap();
    assert_eq!(items.len(), 2, "unpublished items are not listed: {page}");
    assert_eq!(items[0]["slug"], json!("blue-mug"), "ranking first");
    assert_eq!(dec(&items[0]["price"]["list_price"]), n("250"));
    assert_eq!(dec(&items[0]["price"]["price_with_tax"]), n("295"), "18% VAT");
    assert_eq!(dec(&items[1]["price"]["price_with_tax"]), n("524.48"), "5% item rate: 499.5 x 1.05 = 524.475 rounds half up");
    assert_eq!(items[0]["in_stock"], json!(true));
    assert_eq!(items[0]["image"]["url"], json!("https://cdn.example.com/mug.jpg"));
    let raw = page.to_string();
    for leak in ["valuation", "basic_rate", "\"90\"", "item_tax_rates", "token_hash"] {
        assert!(!raw.contains(leak), "catalog leaks {leak}: {raw}");
    }
    let kitchen_page = t.get_pub("/catalog?category=kitchen-dining").await;
    assert_eq!(kitchen_page["items"].as_array().unwrap().len(), 1, "a category includes its subcategories");
    assert_eq!(t.get_pub("/catalog?q=TEE").await["items"][0]["slug"], json!("logo-tee"), "search by code");
    assert_eq!(t.get_pub("/catalog?max_price=300").await["items"].as_array().unwrap().len(), 1);
    let first = t.get_pub("/catalog?sort=price_desc&limit=1").await;
    assert_eq!(first["items"][0]["slug"], json!("logo-tee"));
    let cursor = first["next_cursor"].as_str().expect("more").to_string();
    let second = t.get_pub(&format!("/catalog?sort=price_desc&limit=1&cursor={cursor}")).await;
    assert_eq!(second["items"][0]["slug"], json!("blue-mug"));
    assert!(second["next_cursor"].is_null());
    assert!(matches!(t.call(None, "GET", &format!("/catalog?sort=name&cursor={cursor}"), &[], None).await, Err(Error::Invalid(_))), "cursor of another sort");
    let mug_page = t.get_pub("/items/blue-mug").await;
    assert_eq!(mug_page["sku"], json!("MUG"));
    assert_eq!(mug_page["unit"]["whole_number"], json!(true));
    assert_eq!(dec(&mug_page["available_qty"]), n("10"));
    assert_eq!(mug_page["categories"][0]["slug"], json!("mugs"));
    let tee_page = t.get_pub("/items/logo-tee").await;
    assert_eq!(tee_page["related"][0]["slug"], json!("blue-mug"));
    assert_eq!(dec(&tee_page["max_qty"]), n("5"));
    assert!(matches!(t.call(None, "GET", "/items/secret-prototype", &[], None).await, Err(Error::NotFound(_))), "unpublished");

    // ───────────── cart: server prices, refusals ─────────────
    let (a, cart) = t.new_cart(None, json!([{"item": "blue-mug", "qty": 2}])).await;
    assert_eq!(cart["items"][0]["qty"], json!("2"));
    assert_eq!(cart["items"][0]["available"], json!(true));
    assert!(cart["missing"].as_array().unwrap().contains(&json!("email")));
    let cart = t
        .cart_ok(&a, "POST", "/cart/items", Some(json!({"item": "logo-tee", "qty": "1", "price": "1", "price_list_rate": "1", "rate": "1", "amount": "1"})))
        .await;
    let tee_line = cart["items"].as_array().unwrap().iter().find(|l| l["item"] == json!("logo-tee")).unwrap().clone();
    assert_eq!(dec(&tee_line["price"]), n("499.5"), "client-sent prices are ignored");
    let refused = t.cart(&a, "PUT", "/cart/details", Some(json!({"grand_total": "1"}))).await;
    assert_eq!(field_error(&refused, "grand_total"), "unknown field");
    let unpublished = t.cart(&a, "POST", "/cart/items", Some(json!({"item": "secret-prototype"}))).await;
    assert_eq!(field_error(&unpublished, "item"), "this item is not available");
    let too_many = t.cart(&a, "POST", "/cart/items", Some(json!({"item": "blue-mug", "qty": 9}))).await;
    assert_eq!(field_error(&too_many, "qty"), "only 10 available", "2 in the cart + 9 > 10 in stock");
    let over_limit = t.cart(&a, "PUT", &format!("/cart/items/{}", tee_line["id"].as_str().unwrap()), Some(json!({"qty": 6}))).await;
    assert_eq!(field_error(&over_limit, "qty"), "at most 5 per order");
    let over_stock = t.cart(&a, "PUT", &format!("/cart/items/{}", tee_line["id"].as_str().unwrap()), Some(json!({"qty": 4}))).await;
    assert_eq!(field_error(&over_stock, "qty"), "only 3 available");
    let fraction = t.cart(&a, "POST", "/cart/items", Some(json!({"item": "blue-mug", "qty": "0.5"}))).await;
    assert_eq!(field_error(&fraction, "qty"), "must be a whole number");
    assert!(matches!(t.cart("not-a-token", "GET", "/cart", None).await, Err(Error::NotFound(_))));

    // Totals before an address: no shipping rule applies yet.
    let cart = t.cart_ok(&a, "GET", "/cart", None).await;
    assert_eq!(dec(&cart["totals"]["subtotal"]), n("999.5"));
    assert_eq!(dec(&cart["totals"]["total_taxes"]), n("114.98"), "90 + 24.975 rounded once per tax row");
    assert_eq!(dec(&cart["totals"]["grand_total"]), n("1114.48"));
    assert!(cart["missing"].as_array().unwrap().contains(&json!("shipping_method")));
    let bad_country = t.cart(&a, "PUT", "/cart/details", Some(json!({"shipping_address": address("US")}))).await;
    assert_eq!(field_error(&bad_country, "shipping_address.country"), "we do not ship to this country");
    let bad_email = t.cart(&a, "PUT", "/cart/details", Some(json!({"email": "not-an-email"}))).await;
    assert_eq!(field_error(&bad_email, "email"), "not a valid email address");

    // Germany: weight bands (0.4 x 2 + 0.2 = 1.0 kg, in [1, 5) → 900).
    let cart = t.cart_ok(&a, "PUT", "/cart/details", Some(details(GUEST, "Asha Rao", "DE"))).await;
    assert_eq!(cart["shipping"]["label"], json!("Courier (Germany)"), "the only applicable rule is chosen");
    assert_eq!(dec(&cart["totals"]["shipping"]), n("900"));
    let options = t.cart_ok(&a, "GET", "/cart/shipping-options", None).await;
    assert_eq!(options["options"].as_array().unwrap().len(), 1);
    // India: amount bands, 999.5 < 1000 → 80.
    let cart = t.cart_ok(&a, "PUT", "/cart/details", Some(json!({"shipping_address": address("IN")}))).await;
    assert_eq!(cart["shipping"]["id"], json!(sid(&standard)));
    assert_eq!(dec(&cart["totals"]["shipping"]), n("80"));
    let wrong_rule = t.cart(&a, "PUT", "/cart/shipping", Some(json!({"shipping_method": sid(&courier)}))).await;
    assert_eq!(field_error(&wrong_rule, "shipping_method"), "this shipping method is not available for your cart and address");
    assert_eq!(dec(&cart["totals"]["total_taxes"]), n("129.38"), "shipping is taxed: 90 + 24.975 + 14.4");
    assert_eq!(dec(&cart["totals"]["grand_total"]), n("1208.88"));
    assert_eq!(cart["missing"], json!([]));
    // Free from 1000: a third mug drops the shipping line; back to two brings it back.
    let mug_line = cart["items"].as_array().unwrap().iter().find(|l| l["item"] == json!("blue-mug")).unwrap()["id"].as_str().unwrap().to_string();
    let three = t.cart_ok(&a, "PUT", &format!("/cart/items/{mug_line}"), Some(json!({"qty": 3}))).await;
    assert_eq!(dec(&three["totals"]["shipping"]), Decimal::ZERO);
    assert_eq!(three["items"].as_array().unwrap().len(), 2, "no shipping line among the items");
    let cart = t.cart_ok(&a, "PUT", &format!("/cart/items/{mug_line}"), Some(json!({"qty": 2}))).await;
    assert_eq!(dec(&cart["totals"]["grand_total"]), n("1208.88"));

    // The same lines through the selling engine on a sales order give the same totals.
    let preview =
        t.k.preview(
            &sys,
            "SalesOrder",
            obj(json!({"company": cid.to_string(), "items": [{"item": mug.to_string(), "qty": "2"}, {"item": tee.to_string(), "qty": "1"},
                                                              {"item": ship.to_string(), "qty": "1", "price_list_rate": "80", "discount_percent": "0"}]})),
        )
        .await
        .unwrap();
    assert_eq!(dec(&preview["total_taxes"]), dec(&cart["totals"]["total_taxes"]));
    assert_eq!(dec(&preview["grand_total"]), dec(&cart["totals"]["grand_total"]));

    // ───────────── coupons ─────────────
    assert_eq!(field_error(&t.cart(&a, "PUT", "/cart/coupon", Some(json!({"code": "NOPE1"}))).await, "coupon_code"), "this coupon code is not valid");
    assert_eq!(field_error(&t.cart(&a, "PUT", "/cart/coupon", Some(json!({"code": "old50"}))).await, "coupon_code"), "this coupon has expired");
    assert_eq!(
        field_error(&t.cart(&a, "PUT", "/cart/coupon", Some(json!({"code": "PROTO5"}))).await, "coupon_code"),
        "this coupon does not apply to the items in your cart"
    );
    let cart = t.cart_ok(&a, "PUT", "/cart/coupon", Some(json!({"code": "mug10"}))).await;
    assert_eq!(cart["coupon"]["code"], json!("MUG10"));
    let mug_view = cart["items"].as_array().unwrap().iter().find(|l| l["item"] == json!("blue-mug")).unwrap().clone();
    assert_eq!((dec(&mug_view["list_price"]), dec(&mug_view["price"])), (n("250"), n("225")));
    assert_eq!(dec(&cart["totals"]["discount"]), n("50"));
    assert_eq!(dec(&cart["totals"]["total_taxes"]), n("120.38"), "81 + 24.975 + 14.4");
    assert_eq!(dec(&cart["totals"]["grand_total"]), n("1149.88"));

    // ───────────── checkout ─────────────
    let no_terms = t.checkout(None, &a, "order-a-0001", json!({"payment_method": "fakepay"})).await;
    assert_eq!(field_error(&no_terms, "accept_terms"), "accept the terms to place the order");
    let no_method = t.checkout(None, &a, "order-a-0001", json!({"payment_method": "bitcoin", "accept_terms": true})).await;
    field_error(&no_method, "payment_method");
    let short_key = t.checkout(None, &a, "short", json!({"payment_method": "fakepay", "accept_terms": true})).await;
    field_error(&short_key, "idempotency_key");
    let stale = t.checkout(None, &a, "order-a-0001", json!({"payment_method": "fakepay", "accept_terms": true, "expected_total": "1208.88"})).await;
    assert!(matches!(stale, Err(Error::Conflict(ref m)) if m.contains("1149.88")), "{stale:?}");
    let evil = t.checkout(None, &a, "order-a-0001", json!({"payment_method": "fakepay", "accept_terms": true, "return_url": "https://evil.example/x"})).await;
    field_error(&evil, "return_url");
    let body_a = json!({"payment_method": "fakepay", "accept_terms": true, "expected_total": "1149.88", "return_url": "https://shop.example.com/thanks"});
    let placed = t.checkout(None, &a, "order-a-0001", body_a.clone()).await.expect("checkout");
    let number_a = placed["order"]["number"].as_str().unwrap().to_string();
    let token_a = placed["order_token"].as_str().unwrap().to_string();
    let pr_a: Uuid = placed["payment_request"]["id"].as_str().unwrap().parse().unwrap();
    assert_eq!(placed["payment_request"]["status"], json!("Initiated"));
    assert_eq!(dec(&placed["payment_request"]["amount"]), n("1149.88"));
    assert_eq!(placed["payment_request"]["currency"], json!("INR"));
    assert_eq!(placed["order"]["payment_status"], json!("Pending"));
    assert_eq!(dec(&placed["order"]["totals"]["grand_total"]), n("1149.88"));
    assert_eq!(dec(&placed["order"]["shipping"]["amount"]), n("80"));
    assert_eq!(placed["order"]["items"].as_array().unwrap().len(), 2);
    // Same key: the same order, once.
    let again = t.checkout(None, &a, "order-a-0001", body_a.clone()).await.expect("replay");
    assert_eq!(again["order"]["number"], json!(number_a));
    assert_eq!(again["order_token"], json!(token_a));
    assert_eq!(t.count("SalesOrder", json!({"number": number_a})).await, 1);
    assert_eq!(t.count("PaymentRequest", json!({"order_number": number_a})).await, 1);
    let other_key = t.checkout(None, &a, "order-a-0002", body_a.clone()).await;
    assert!(matches!(other_key, Err(Error::Conflict(_))), "{other_key:?}");
    let frozen = t.cart(&a, "POST", "/cart/items", Some(json!({"item": "blue-mug"}))).await;
    assert!(matches!(frozen, Err(Error::Conflict(_))), "an ordered cart is frozen: {frozen:?}");
    let so_a = t.find_one("SalesOrder", json!({"number": number_a})).await;
    let so_a = t.get("SalesOrder", uid(&so_a)).await;
    assert_eq!(so_a["docstatus"], json!(1));
    assert_eq!(so_a["status"], json!("To Deliver and Bill"));
    assert_eq!(so_a["commerce_web_order"], json!(true));
    assert_eq!(so_a["commerce_contact_email"], json!(GUEST));
    assert_eq!(dec(&so_a["grand_total"]), n("1149.88"), "the order is priced exactly like the cart");
    let guest_a = t.get("Customer", so_a["customer"].as_str().unwrap().parse().unwrap()).await;
    assert_eq!(guest_a["commerce_guest"], json!(true));
    assert_eq!(guest_a["customer_group"], individual["id"]);
    assert_eq!(dec(&t.get("CouponCode", uid(&mug10)).await["used_count"]), Decimal::ONE);
    assert_eq!(t.available("blue-mug").await, n("8"), "the order reserves its stock");
    let told: Vec<String> = {
        let mut tx = t.k.db.tenant_tx(tenant).await.unwrap();
        sqlx::query_scalar("select title from notification where tenant_id = $1 and user_id = $2").bind(tenant).bind(manager).fetch_all(&mut *tx).await.unwrap()
    };
    assert_eq!(told, vec![format!("New web order {number_a}")], "the store's managers hear about new web orders");

    // ───────────── coupon limits ─────────────
    let (b, _) = t.new_cart(None, json!([{"item": "blue-mug", "qty": 1}])).await;
    assert_eq!(field_error(&t.cart(&b, "PUT", "/cart/coupon", Some(json!({"code": "MUG10"}))).await, "coupon_code"), "this coupon has been used up");

    // ───────────── payment through the gateway's action ─────────────
    let looked = t.ok(None, "GET", &format!("/payment-requests/{pr_a}"), &[("x-order-token", token_a.as_str())], None).await;
    assert!(looked["payment_url"].is_null());
    let bad_token = t.call(None, "GET", &format!("/payment-requests/{pr_a}"), &[("x-order-token", "AAAA")], None).await;
    assert!(matches!(bad_token, Err(Error::NotFound(_))));
    t.k.run_action(
        &fakepay,
        "PaymentRequest",
        pr_a,
        "set_gateway_details",
        json!({"payment_url": "https://pay.fakepay.test/c/123", "gateway_reference": "cs_123"}),
    )
    .await
    .expect("gateway link");
    let looked = t.ok(None, "GET", &format!("/payment-requests/{pr_a}"), &[("x-order-token", token_a.as_str())], None).await;
    assert_eq!(looked["payment_url"], json!("https://pay.fakepay.test/c/123"));
    let foreign = t.k.run_action(&otherpay, "PaymentRequest", pr_a, "mark_paid", json!({"amount": "1149.88", "currency": "INR"})).await;
    assert!(matches!(foreign, Err(Error::Forbidden(_))), "another gateway cannot settle it: {foreign:?}");
    let short =
        t.k.run_action(&fakepay, "PaymentRequest", pr_a, "mark_paid", json!({"amount": "1149.00", "currency": "INR", "gateway_reference": "pi_1"})).await;
    assert!(matches!(short, Err(Error::Invalid(ref m)) if m.contains("does not match")), "amount mismatch refused: {short:?}");
    let usd = t.k.run_action(&fakepay, "PaymentRequest", pr_a, "mark_paid", json!({"amount": "1149.88", "currency": "USD", "gateway_reference": "pi_1"})).await;
    assert!(matches!(usd, Err(Error::Invalid(_))), "currency mismatch refused: {usd:?}");
    let paid =
        t.k.run_action(&fakepay, "PaymentRequest", pr_a, "mark_paid", json!({"amount": "1149.88", "currency": "inr", "gateway_reference": "pi_1"}))
            .await
            .expect("paid");
    assert_eq!(paid["status"], json!("Paid"));
    assert_eq!(paid["replayed"], json!(false));
    let pe_a: Uuid = paid["payment_entry"].as_str().unwrap().parse().unwrap();
    let replay =
        t.k.run_action(&fakepay, "PaymentRequest", pr_a, "mark_paid", json!({"amount": "1149.88", "currency": "INR", "gateway_reference": "pi_1"}))
            .await
            .expect("replay");
    assert_eq!(replay["replayed"], json!(true));
    assert_eq!(replay["payment_entry"], json!(pe_a.to_string()));
    assert_eq!(t.count("PaymentEntry", json!({"commerce_payment_request": pr_a.to_string()})).await, 1, "confirmed twice, booked once");
    let double =
        t.k.run_action(&fakepay, "PaymentRequest", pr_a, "mark_paid", json!({"amount": "1149.88", "currency": "INR", "gateway_reference": "pi_2"})).await;
    assert!(matches!(double, Err(Error::Conflict(_))), "a second, different payment is flagged: {double:?}");
    let pe = t.get("PaymentEntry", pe_a).await;
    assert_eq!(pe["docstatus"], json!(1));
    assert_eq!(dec(&pe["paid_amount"]), n("1149.88"));
    assert_eq!(dec(&pe["unallocated_amount"]), n("1149.88"), "an advance on the order");
    assert_eq!(pe["commerce_sales_order"], so_a["id"]);
    assert_eq!(t.get("SalesOrder", uid(&so_a)).await["commerce_payment_status"], json!("Paid"));
    let cancel_paid = t.k.cancel(&t.ctx, "SalesOrder", uid(&so_a), None).await;
    assert!(matches!(cancel_paid, Err(Error::Invalid(ref m)) if m.contains("paid")), "{cancel_paid:?}");

    // Delivery and invoice: the advance settles the invoice.
    let dn = t.k.run_action(&t.ctx, "SalesOrder", uid(&so_a), "make_delivery_note", json!({})).await.unwrap();
    t.submit("DeliveryNote", dn["id"].as_str().unwrap().parse().unwrap()).await;
    let si = t.k.run_action(&t.ctx, "SalesOrder", uid(&so_a), "make_sales_invoice", json!({})).await.unwrap();
    let si = t.submit("SalesInvoice", si["id"].as_str().unwrap().parse().unwrap()).await;
    assert_eq!(dec(&si["grand_total"]), n("1149.88"));
    let si = t.get("SalesInvoice", uid(&si)).await;
    assert_eq!(dec(&si["outstanding_amount"]), Decimal::ZERO, "the web payment is applied");
    assert_eq!(si["status"], json!("Paid"));
    assert_eq!(t.get("SalesOrder", uid(&so_a)).await["status"], json!("Completed"));
    assert_eq!(t.available("blue-mug").await, n("8"), "delivered stock left; nothing reserved any more");

    // ───────────── a guest using an existing customer's email ─────────────
    let priya = t
        .insert(
            "Customer",
            json!({"customer_name": "Priya Nair", "email": "priya@example.com", "billing_address": "7 Secret Lane, Kochi", "customer_group": individual["id"]}),
        )
        .await;
    let staff_so = t
        .insert(
            "SalesOrder",
            json!({"company": cid.to_string(), "customer": sid(&priya), "items": [{"item": mug.to_string(), "qty": "1", "warehouse": stores["id"]}]}),
        )
        .await;
    let staff_so = t.submit("SalesOrder", uid(&staff_so)).await;
    let (c, _) = t.new_cart(None, json!([{"item": "blue-mug", "qty": 1}])).await;
    t.cart_ok(&c, "PUT", "/cart/details", Some(details("Priya@Example.com", "Not Priya", "IN"))).await;
    let placed_c = t.checkout(None, &c, "order-c-0001", json!({"payment_method": "Cash on Delivery", "accept_terms": true})).await.expect("guest checkout");
    let raw = placed_c.to_string();
    assert!(!raw.contains("Secret Lane") && !raw.contains("Priya Nair"), "nothing of the existing customer leaks: {raw}");
    assert_eq!(placed_c["payment_request"]["instructions"], json!("Pay the courier."));
    let so_c = t.find_one("SalesOrder", json!({"number": placed_c["order"]["number"]})).await;
    assert_ne!(so_c["customer"], priya["id"], "not attached to the existing customer");
    let guest_c = t.get("Customer", so_c["customer"].as_str().unwrap().parse().unwrap()).await;
    assert_eq!(guest_c["commerce_guest"], json!(true));
    assert_eq!(guest_c["email"], json!("priya@example.com"));
    let priya_after = t.get("Customer", uid(&priya)).await;
    assert_eq!((priya_after["customer_name"].clone(), priya_after["billing_address"].clone()), (json!("Priya Nair"), json!("7 Secret Lane, Kochi")));

    // ───────────── portal customers see only their own orders ─────────────
    let mine = t.ok(Some(&priya_user), "GET", "/orders", &[], None).await;
    let numbers: Vec<&Value> = mine["orders"].as_array().unwrap().iter().map(|o| &o["number"]).collect();
    assert_eq!(numbers, vec![&staff_so["number"]], "Priya sees her account's order, not the guest order placed with her email");
    let not_hers = t.call(Some(&priya_user), "GET", &format!("/orders/{}", placed_c["order"]["number"].as_str().unwrap()), &[], None).await;
    assert!(matches!(not_hers, Err(Error::NotFound(_))));
    assert!(matches!(t.call(None, "GET", "/orders", &[], None).await, Err(Error::Unauthorized(_))));
    // Rahul shops signed in: his checkout makes him a (verified) customer.
    let (r, _) = t.new_cart(Some(&rahul), json!([{"item": "logo-tee", "qty": 1}])).await;
    t.ok(Some(&rahul), "PUT", "/cart/details", &[("x-cart-token", r.as_str())], Some(details("rahul@example.com", "Rahul Menon", "IN"))).await;
    let placed_r =
        t.checkout(Some(&rahul), &r, "order-r-0001", json!({"payment_method": "Cash on Delivery", "accept_terms": true})).await.expect("portal checkout");
    let number_r = placed_r["order"]["number"].as_str().unwrap().to_string();
    let rahul_customer = t.find_one("Customer", json!({"email": "rahul@example.com"})).await;
    assert_eq!(rahul_customer["commerce_guest"], json!(false));
    let his = t.ok(Some(&rahul), "GET", "/orders", &[], None).await;
    assert_eq!(his["orders"].as_array().unwrap().len(), 1);
    assert_eq!(his["orders"][0]["number"], json!(number_r));
    assert!(t.ok(Some(&rahul), "GET", &format!("/orders/{number_r}"), &[], None).await["order"]["items"][0]["title"] == json!("Logo Tee"));
    let foreign = t.call(Some(&rahul), "GET", &format!("/orders/{}", staff_so["number"].as_str().unwrap()), &[], None).await;
    assert!(matches!(foreign, Err(Error::NotFound(_))), "another customer's order: {foreign:?}");
    assert_eq!(t.ok(Some(&rahul), "GET", "/invoices", &[], None).await["invoices"], json!([]));
    let again = t.ok(Some(&rahul), "POST", &format!("/orders/{number_r}/reorder"), &[], None).await;
    assert_eq!(again["cart"]["items"][0]["item"], json!("logo-tee"));
    assert!(again["token"].as_str().is_some_and(|tk| tk.len() == 43));
    // A second signed-in checkout reuses his customer.
    t.cart_ok(again["token"].as_str().unwrap(), "PUT", "/cart/details", Some(details("rahul@example.com", "Rahul Menon", "IN"))).await;

    // ───────────── guest order lookup ─────────────
    let lookup = t.ok(None, "POST", "/orders/lookup", &[], Some(json!({"number": number_a, "email": "ASHA@example.com", "token": token_a}))).await;
    assert_eq!(lookup["order"]["payment_status"], json!("Paid"));
    for (email, tok) in [("someone@example.com", token_a.as_str()), (GUEST, placed_c["order_token"].as_str().unwrap())] {
        let miss = t.call(None, "POST", "/orders/lookup", &[], Some(json!({"number": number_a, "email": email, "token": tok}))).await;
        assert!(matches!(miss, Err(Error::NotFound(_))), "{miss:?}");
    }

    // ───────────── two checkouts racing for the last tee ─────────────
    // 3 tees: 1 delivered on order A, 1 reserved by Rahul's order, 1 left.
    assert_eq!(t.available("logo-tee").await, n("1"));
    let (x, _) = t.new_cart(None, json!([{"item": "logo-tee", "qty": 1}])).await;
    let (y, _) = t.new_cart(None, json!([{"item": "logo-tee", "qty": 1}])).await;
    t.cart_ok(&x, "PUT", "/cart/details", Some(details("x@example.com", "X", "IN"))).await;
    t.cart_ok(&y, "PUT", "/cart/details", Some(details("y@example.com", "Y", "IN"))).await;
    let cod = json!({"payment_method": "Cash on Delivery", "accept_terms": true});
    let (rx, ry) = tokio::join!(t.checkout(None, &x, "race-x-0001", cod.clone()), t.checkout(None, &y, "race-y-0001", cod.clone()));
    assert_eq!([rx.is_ok(), ry.is_ok()].iter().filter(|ok| **ok).count(), 1, "exactly one gets the last tee: {rx:?} / {ry:?}");
    let loser = if rx.is_ok() { ry } else { rx };
    assert!(field_error(&loser, "items").contains("out of stock"), "{loser:?}");
    assert_eq!(t.available("logo-tee").await, Decimal::ZERO);
    assert_eq!(t.get_pub("/items/logo-tee").await["in_stock"], json!(false));
    assert_eq!(t.get_pub("/catalog?in_stock=1").await["items"].as_array().unwrap().len(), 1, "in-stock filter");

    // ───────────── an expired payment request cannot be paid; upkeep releases the stock ─────────────
    let (e, _) = t.new_cart(None, json!([{"item": "blue-mug", "qty": 3}])).await;
    t.cart_ok(&e, "PUT", "/cart/details", Some(details("late@example.com", "Late Payer", "IN"))).await;
    let placed_e = t.checkout(None, &e, "order-e-0001", json!({"payment_method": "fakepay", "accept_terms": true})).await.expect("checkout e");
    let pr_e: Uuid = placed_e["payment_request"]["id"].as_str().unwrap().parse().unwrap();
    assert!(placed_e["payment_request"]["expires_at"].is_string());
    let before = t.available("blue-mug").await;
    t.sql(&format!("update e_payment_request set expires_at = now() - interval '1 minute' where id = '{pr_e}'")).await;
    let late = t.k.run_action(&fakepay, "PaymentRequest", pr_e, "mark_paid", json!({"amount": placed_e["payment_request"]["amount"], "currency": "INR"})).await;
    assert!(matches!(late, Err(Error::Invalid(ref m)) if m.contains("expired")), "{late:?}");
    ferrum_app_commerce::maintenance(&t.k).await.expect("upkeep");
    assert_eq!(t.get("PaymentRequest", pr_e).await["status"], json!("Expired"));
    let so_e = t.find_one("SalesOrder", json!({"number": placed_e["order"]["number"]})).await;
    assert_eq!(so_e["docstatus"], json!(2), "the unpaid order is cancelled");
    assert_eq!((so_e["status"].clone(), so_e["commerce_payment_status"].clone()), (json!("Cancelled"), json!("Expired")));
    assert_eq!(t.available("blue-mug").await, before + n("3"), "its reservation is released");
    let still_expired =
        t.k.run_action(&fakepay, "PaymentRequest", pr_e, "mark_paid", json!({"amount": placed_e["payment_request"]["amount"], "currency": "INR"})).await;
    assert!(matches!(still_expired, Err(Error::Invalid(_))));

    // ───────────── a failed payment, paid again by another method ─────────────
    let (f, _) = t.new_cart(None, json!([{"item": "blue-mug", "qty": 1}])).await;
    t.cart_ok(&f, "PUT", "/cart/details", Some(details("retry@example.com", "Retry", "IN"))).await;
    let placed_f = t.checkout(None, &f, "order-f-0001", json!({"payment_method": "fakepay", "accept_terms": true})).await.expect("checkout f");
    let pr_f: Uuid = placed_f["payment_request"]["id"].as_str().unwrap().parse().unwrap();
    t.k.run_action(&fakepay, "PaymentRequest", pr_f, "mark_failed", json!({"reason": "card declined"})).await.expect("failed");
    let number_f = placed_f["order"]["number"].as_str().unwrap().to_string();
    let token_f = placed_f["order_token"].as_str().unwrap().to_string();
    let lookup = t.ok(None, "POST", "/orders/lookup", &[], Some(json!({"number": number_f, "email": "retry@example.com", "token": token_f}))).await;
    assert_eq!(lookup["order"]["payment_status"], json!("Failed"));
    let retry = t
        .ok(
            None,
            "POST",
            "/orders/pay",
            &[],
            Some(json!({"number": number_f, "email": "retry@example.com", "token": token_f, "payment_method": "Cash on Delivery"})),
        )
        .await;
    assert_eq!(retry["payment_request"]["status"], json!("Initiated"));
    assert_eq!(retry["payment_request"]["gateway"], json!("Cash on Delivery"));
    let so_f = t.find_one("SalesOrder", json!({"number": number_f})).await;
    assert_eq!(so_f["commerce_payment_status"], json!("Pending"));
    // Staff record the cash when the courier hands it over.
    let pr_cod: Uuid = retry["payment_request"]["id"].as_str().unwrap().parse().unwrap();
    let plugin_cod =
        t.k.run_action(&fakepay, "PaymentRequest", pr_cod, "mark_paid", json!({"amount": retry["payment_request"]["amount"], "currency": "INR"})).await;
    assert!(matches!(plugin_cod, Err(Error::Forbidden(_))), "plugins never settle cash on delivery");
    t.k.run_action(&t.ctx, "PaymentRequest", pr_cod, "mark_paid", json!({"amount": retry["payment_request"]["amount"], "currency": "INR"}))
        .await
        .expect("cash");
    assert_eq!(t.get("SalesOrder", uid(&so_f)).await["commerce_payment_status"], json!("Paid"));

    // ───────────── reviews: visible once approved ─────────────
    let review = t
        .ok(
            None,
            "POST",
            "/items/blue-mug/reviews",
            &[],
            Some(json!({"rating": 5, "title": "Lovely", "review": "Keeps tea hot.", "name": "Asha", "email": GUEST})),
        )
        .await;
    assert_eq!(review["status"], json!("Pending"));
    let bad_rating = t.call(None, "POST", "/items/blue-mug/reviews", &[], Some(json!({"rating": 6, "name": "X", "email": "x@example.com"}))).await;
    assert_eq!(field_error(&bad_rating, "rating"), "rate the item from 1 to 5");
    let twice = t.call(None, "POST", "/items/blue-mug/reviews", &[], Some(json!({"rating": 4, "name": "Asha", "email": GUEST}))).await;
    assert!(matches!(twice, Err(Error::Conflict(_))));
    let stranger = t.ok(None, "POST", "/items/blue-mug/reviews", &[], Some(json!({"rating": 1, "name": "Troll", "email": "troll@example.com"}))).await;
    assert_eq!(stranger["status"], json!("Pending"));
    assert_eq!(t.get_pub("/items/blue-mug").await["reviews"], json!([]), "pending reviews are hidden");
    let r1 = t.find_one("ItemReview", json!({"reviewer_email": GUEST})).await;
    assert_eq!(r1["verified_purchase"], json!(true), "Asha bought a mug");
    assert_eq!(t.find_one("ItemReview", json!({"reviewer_email": "troll@example.com"})).await["verified_purchase"], json!(false));
    t.k.update(&t.ctx, "ItemReview", uid(&r1), obj(json!({"status": "Approved"})), None).await.unwrap();
    let page = t.get_pub("/items/blue-mug").await;
    assert_eq!(page["reviews"].as_array().unwrap().len(), 1);
    assert_eq!(page["reviews"][0]["verified_purchase"], json!(true));
    assert!(!page.to_string().contains(GUEST), "reviewer emails are never public");
    assert_eq!((page["rating"]["average"].clone(), page["rating"]["count"].clone()), (json!("5"), json!(1)));
    assert_eq!(t.get_pub("/items/blue-mug/reviews").await["reviews"].as_array().unwrap().len(), 1);

    // ───────────── abandoned and expired carts ─────────────
    let (g, _) = t.new_cart(None, json!([{"item": "blue-mug", "qty": 1}])).await;
    t.cart_ok(&g, "PUT", "/cart/details", Some(json!({"email": "idle@example.com"}))).await;
    let (h, _) = t.new_cart(None, json!([{"item": "blue-mug", "qty": 1}])).await;
    let hash = |tok: &str| {
        use sha2::Digest;
        hex::encode(sha2::Sha256::digest(tok.as_bytes()))
    };
    t.sql(&format!("update e_shopping_cart set last_activity = now() - interval '2 days' where token_hash = '{}'", hash(&g))).await;
    t.sql(&format!("update e_shopping_cart set last_activity = now() - interval '40 days' where token_hash = '{}'", hash(&h))).await;
    ferrum_app_commerce::maintenance(&t.k).await.expect("upkeep");
    let idle = t.find_one("ShoppingCart", json!({"email": "idle@example.com"})).await;
    assert_eq!(idle["status"], json!("Abandoned"));
    let events: i64 = {
        let mut tx = t.k.db.tenant_tx(tenant).await.unwrap();
        sqlx::query_scalar("select count(*) from outbox where tenant_id = $1 and topic = 'commerce.cart_abandoned'")
            .bind(tenant)
            .fetch_one(&mut *tx)
            .await
            .unwrap()
    };
    assert_eq!(events, 1, "contactable abandoned carts announce themselves");
    let expired = t.cart(&h, "POST", "/cart/items", Some(json!({"item": "blue-mug"}))).await;
    assert!(matches!(expired, Err(Error::Conflict(ref m)) if m.contains("expired")), "{expired:?}");
    assert_eq!(t.cart_ok(&g, "POST", "/cart/items", Some(json!({"item": "blue-mug"}))).await["status"], json!("Open"), "a returning shopper revives the cart");

    // ───────────── guest checkout can be switched off ─────────────
    t.k.update(&t.ctx, "StoreSettings", uid(&settings), obj(json!({"allow_guest_checkout": false})), None).await.unwrap();
    t.cart_ok(&g, "PUT", "/cart/details", Some(details("idle@example.com", "Idle", "IN"))).await;
    assert!(matches!(t.checkout(None, &g, "order-g-0001", cod.clone()).await, Err(Error::Forbidden(_))));

    // Sales users find web orders among the sales orders.
    let q = ListQuery { filters: ferrum_kernel::query::parse_filters(&json!({"commerce_web_order": true})).unwrap(), ..Default::default() };
    assert_eq!(t.k.list(&sales_user, "SalesOrder", &q).await.unwrap().rows.len(), 6, "A, C, Rahul's, the race winner, E (cancelled) and F");

    // ───────────── reports ─────────────
    let today = chrono::Utc::now().date_naive().to_string();
    let p = [("company", cid.to_string()), ("from_date", today.clone()), ("to_date", today.clone())];
    let params: Vec<(&str, &str)> = p.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let by_day = t.report("web_sales", &params).await;
    let web_orders = by_day["totals"]["orders"].as_i64().unwrap();
    assert_eq!(web_orders, 5, "A, C, Rahul's, the race winner and F (E was cancelled): {by_day}");
    let mut by_item = params.clone();
    by_item.push(("group_by", "Item"));
    let items_report = t.report("web_sales", &by_item).await;
    assert!(items_report["rows"].as_array().unwrap().iter().all(|r| r["name"] != json!("Shipping")), "no shipping line among items");
    let mut by_cat = params.clone();
    by_cat.push(("group_by", "Category"));
    assert!(t.report("web_sales", &by_cat).await["rows"].as_array().unwrap().iter().any(|r| r["name"] == json!("Mugs")));
    let top = t.report("top_products", &params).await;
    assert_eq!((top["rows"][0]["item_code"].clone(), dec(&top["rows"][0]["revenue"])), (json!("TEE"), n("1498.5")), "3 tees at 499.5: {top}");
    assert_eq!((top["rows"][1]["item_code"].clone(), dec(&top["rows"][1]["units"])), (json!("MUG"), n("4")), "2 on A, 1 on C, 1 on F (E was cancelled)");
    let since = (chrono::Utc::now().date_naive() - chrono::Duration::days(60)).to_string();
    let abandoned = t.report("abandoned_carts", &[("company", cid.to_string().as_str()), ("from_date", since.as_str()), ("to_date", today.as_str())]).await;
    assert_eq!(abandoned["totals"]["carts"], json!(1), "cart H, idle for 40 days; G came back: {abandoned}");
    assert_eq!((abandoned["rows"][0]["status"].clone(), abandoned["rows"][0]["contactable"].clone()), (json!("Expired"), json!(false)));
    assert_eq!(dec(&abandoned["rows"][0]["value"]), n("295"), "a mug with VAT (no address, so no shipping yet)");
    let funnel = t.report("conversion_funnel", &params).await;
    assert_eq!(funnel["rows"][2]["carts"], json!(6), "checked out: A, C, Rahul, race winner, E, F");
    assert_eq!(funnel["rows"][3]["carts"], json!(2), "paid: A and F");
    let coupons = t.report("coupon_usage", &params).await;
    let mug10_row = coupons["rows"].as_array().unwrap().iter().find(|r| r["code"] == json!("MUG10")).unwrap().clone();
    assert_eq!((mug10_row["orders"].clone(), mug10_row["used_count"].clone()), (json!(1), json!(1)));
    assert_eq!(dec(&mug10_row["discount"]), n("50"));

    // ───────────── with CRM installed: contacts and addresses; portal links through contacts ─────────────
    // (The CRM package is optional for commerce; this part runs when the tree ships it.)
    if t.k.registry.catalog().package("crm").is_none() {
        return;
    }
    t.k.install_packages(tenant, &["crm".to_string()]).await.expect("install crm");
    t.k.update(&t.ctx, "StoreSettings", uid(&settings), obj(json!({"allow_guest_checkout": true})), None).await.unwrap();
    let (z, _) = t.new_cart(None, json!([{"item": "blue-mug", "qty": 1}])).await;
    let mut d = details("meera@example.com", "Meera Iyer", "IN");
    d["billing_address"] = json!({"line1": "1 Office Park", "city": "Chennai", "country": "IN"});
    t.cart_ok(&z, "PUT", "/cart/details", Some(d)).await;
    let placed_z = t.checkout(None, &z, "order-z-0001", cod.clone()).await.expect("checkout with crm");
    let so_z = t.find_one("SalesOrder", json!({"number": placed_z["order"]["number"]})).await;
    let customer_z = so_z["customer"].as_str().unwrap().to_string();
    let ctx2 = t.k.load_ctx(tenant, session.user, None, "it-crm".into()).await.unwrap();
    let crm = T { k: t.k.clone(), ctx: ctx2, site: t.site.clone() };
    let contact = crm.find_one("Contact", json!({"customer": customer_z})).await;
    assert_eq!((contact["first_name"].clone(), contact["last_name"].clone()), (json!("Meera"), json!("Iyer")));
    assert_eq!(crm.count("Address", json!({"customer": customer_z})).await, 2, "a shipping and a billing address");
    assert_eq!(crm.find_one("Address", json!({"customer": customer_z, "address_type": "Billing"})).await["city"], json!("Chennai"));
    // A contact of Priya's account with another email gives that person her orders in the portal.
    crm.insert("Contact", json!({"first_name": "Buyer", "customer": sid(&priya), "emails": [{"email": "buyer@corp.example", "is_primary": true}]})).await;
    let buyer = portal_user("buyer@corp.example").await;
    let theirs = t.ok(Some(&buyer), "GET", "/orders", &[], None).await;
    assert_eq!(theirs["orders"][0]["number"], staff_so["number"]);
    // The guest customer's contact links nobody: guests are never portal accounts.
    let meera = portal_user("meera@example.com").await;
    assert_eq!(t.ok(Some(&meera), "GET", "/orders", &[], None).await["orders"], json!([]));
}
