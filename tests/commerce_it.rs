//! The online store end to end against a real PostgreSQL (`FERRUM_COMMERCE_TEST_DATABASE_URL`,
//! default `postgres://ferrum:ferrum@localhost:5432/ferrum_commerce_test`; the database is reset).
//!
//! Drives the storefront exactly as a browser would — through the package's `Site` handler,
//! public and signed in — and confirms payments the way a gateway plugin does: as its service
//! user, through the payment request's `mark_paid` action.

use ferrum_kernel::secret::SecretKey;
use ferrum_kernel::tenant::NewTenant;
use ferrum_kernel::{Ctx, Db, DbConfig, Doc, Error, HookRegistry, Kernel, KernelSettings, ListQuery, Site, SiteRequest};
use ferrum_meta::{Catalog, PermissionDef};
use rust_decimal::Decimal;
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::{Arc, Mutex};
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

async fn fresh_kernel() -> Arc<Kernel> {
    let url = std::env::var("FERRUM_COMMERCE_TEST_DATABASE_URL").unwrap_or_else(|_| "postgres://ferrum:ferrum@localhost:5432/ferrum_commerce_test".into());
    let db = Db::connect(DbConfig { url, max_connections: 12, ..Default::default() }).await.expect("connect");
    sqlx::raw_sql("drop schema if exists public cascade; create schema public;").execute(&db.pool).await.expect("reset");
    let mut packages = ferrum_domain::packages().expect("platform packages");
    packages.extend(ferrum_app_erp::packages().expect("ERP's packages"));
    packages.extend(ferrum_app_commerce::packages().expect("app packages"));
    packages.extend(ferrum_app_crm::packages().expect("crm packages"));
    packages.extend(ferrum_finance::packages().expect("finance packages"));
    packages.extend(ferrum_stock_ops::packages().expect("stock operations packages"));
    let catalog = Catalog::from_packages(packages).expect("catalog");
    let mut hooks = HookRegistry::default();
    ferrum_domain::register(&mut hooks);
    ferrum_app_erp::register(&mut hooks);
    for source in ferrum_app_erp::reports() {
        ferrum_domain::reports::add_source(source);
    }
    ferrum_app_commerce::register(&mut hooks);
    ferrum_app_crm::register(&mut hooks);
    ferrum_finance::register(&mut hooks);
    ferrum_stock_ops::register(&mut hooks);
    for source in ferrum_app_commerce::reports() {
        ferrum_domain::reports::add_source(source);
    }
    // Messages with a secret value (a gift card's code) are sealed with the server's key while they wait.
    let settings = KernelSettings { secret_key: Some(SecretKey::new(SECRET)), ..Default::default() };
    let k = Kernel::boot(db, catalog, hooks, settings, None).await.expect("boot");
    ferrum_domain::maintenance(&k).await.expect("maintenance");
    ferrum_app_erp::maintenance(&k).await.expect("maintenance");
    k
}

const SECRET: [u8; 32] = [7; 32];

/// The mails queued to `to` with what the outbox holds sealed opened (a secret value's place, otherwise).
async fn opened_mails(t: &T, to: &str) -> Vec<(String, String)> {
    let key = SecretKey::new(SECRET);
    events(t, "ferrum.mail")
        .await
        .iter()
        .filter(|m| m["to"] == json!(to))
        .map(|m| match m["sealed"].as_str() {
            Some(sealed) => {
                let open: Value = serde_json::from_str(&key.open(&format!("{}/message", t.ctx.tenant), sealed).unwrap()).unwrap();
                (open["subject"].as_str().unwrap().to_string(), open["html"].as_str().unwrap().to_string())
            }
            None => (m["subject"].as_str().unwrap_or_default().to_string(), m["html"].as_str().unwrap_or_default().to_string()),
        })
        .collect()
}

/// A gateway's service user, as its plugin acts.
async fn gateway(t: &T, name: &str) -> Ctx {
    let role = format!("plugin:{name}");
    for entity in ["PaymentRequest", "PaymentException", "OrderRefund"] {
        let p = PermissionDef { entity: entity.into(), read: true, write: true, ..Default::default() };
        t.k.set_role_permission(&t.ctx, &role, &p).await.unwrap();
    }
    let u = t.k.create_user(&t.ctx, &format!("{name}@plugins.ferrum.invalid"), &format!("Plugin: {name}"), None, &[role], false).await.unwrap();
    t.k.load_ctx(t.ctx.tenant, u, None, format!("plugin-{name}")).await.unwrap()
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

/// The tests share one database that each of them resets: one at a time.
static DB: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[test]
fn storefront_end_to_end() {
    let _db = DB.lock().unwrap_or_else(|e| e.into_inner());
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
            for entity in ["PaymentRequest", "PaymentException"] {
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

    // ───────────── money a gateway took and could not book ─────────────
    let reported = json!({"gateway_reference": "pi_2", "amount": "1149.88", "currency": "INR", "reason": "it could not be booked: already paid"});
    let as_admin = t.k.run_action(&t.ctx, "PaymentRequest", pr_a, "report_exception", reported.clone()).await;
    assert!(matches!(as_admin, Err(Error::Forbidden(_))), "only a gateway reports one: {as_admin:?}");
    let foreign = t.k.run_action(&otherpay, "PaymentRequest", pr_a, "report_exception", reported.clone()).await;
    assert!(matches!(foreign, Err(Error::Forbidden(_))), "and only for its own requests: {foreign:?}");
    let made = t.k.run_action(&fakepay, "PaymentRequest", pr_a, "report_exception", reported.clone()).await.expect("reported");
    let again = t.k.run_action(&fakepay, "PaymentRequest", pr_a, "report_exception", reported.clone()).await.expect("reported again");
    assert_eq!(made["exception"], again["exception"], "a repeated webhook is one exception");
    let exception: Uuid = made["exception"].as_str().unwrap().parse().unwrap();
    let x = t.get("PaymentException", exception).await;
    assert_eq!((&x["status"], &x["gateway"], &x["gateway_reference"]), (&json!("Open"), &json!("fakepay"), &json!("pi_2")));
    assert_eq!(dec(&x["amount"]), n("1149.88"));
    let own_refund = t.k.run_action(&fakepay, "PaymentException", exception, "approve_refund", json!({})).await;
    assert!(matches!(own_refund, Err(Error::Forbidden(_))), "a gateway never approves its own refund: {own_refund:?}");
    let early = t.k.run_action(&fakepay, "PaymentException", exception, "refund_done", json!({"refund_reference": "re_1"})).await;
    assert!(matches!(early, Err(Error::Conflict(_))), "nor reports one nobody approved: {early:?}");
    let nameless = t.k.run_action(&t.ctx, "PaymentException", exception, "resolve", json!({"note": "  "})).await;
    assert!(matches!(nameless, Err(Error::Validation(_))), "settling it another way needs saying how: {nameless:?}");
    t.k.run_action(&t.ctx, "PaymentException", exception, "approve_refund", json!({})).await.expect("a person approves");
    let approved = t.get("PaymentException", exception).await;
    assert_eq!(approved["status"], json!("Refund Approved"));
    assert_eq!(approved["approved_by"], json!(t.ctx.email));
    let twice = t.k.run_action(&t.ctx, "PaymentException", exception, "approve_refund", json!({})).await;
    assert!(twice.is_err(), "approved once");
    let foreign = t.k.run_action(&otherpay, "PaymentException", exception, "refund_done", json!({"refund_reference": "re_1"})).await;
    assert!(matches!(foreign, Err(Error::Forbidden(_))), "another gateway cannot report it: {foreign:?}");
    t.k.run_action(&fakepay, "PaymentException", exception, "refund_failed", json!({"error": "card closed"})).await.expect("refused at the provider");
    let failed = t.get("PaymentException", exception).await;
    assert_eq!((&failed["status"], &failed["refund_error"]), (&json!("Refund Failed"), &json!("card closed")));
    t.k.run_action(&t.ctx, "PaymentException", exception, "approve_refund", json!({})).await.expect("approved again");
    assert!(t.get("PaymentException", exception).await["refund_error"].is_null());
    t.k.run_action(&fakepay, "PaymentException", exception, "refund_done", json!({"refund_reference": "re_1"})).await.expect("refunded");
    t.k.run_action(&fakepay, "PaymentException", exception, "refund_done", json!({"refund_reference": "re_1"})).await.expect("told twice, one refund");
    let refunded = t.get("PaymentException", exception).await;
    assert_eq!((&refunded["status"], &refunded["refund_reference"]), (&json!("Refunded"), &json!("re_1")));
    let closed = t.k.run_action(&t.ctx, "PaymentException", exception, "approve_refund", json!({})).await;
    assert!(closed.is_err(), "refunded, nothing is left to approve");
    let other =
        t.k.run_action(&fakepay, "PaymentRequest", pr_a, "report_exception", json!({"gateway_reference": "pi_3", "reason": "paid twice"}))
            .await
            .expect("reported");
    let other: Uuid = other["exception"].as_str().unwrap().parse().unwrap();
    t.k.run_action(&t.ctx, "PaymentException", other, "resolve", json!({"note": "Refunded from the Stripe dashboard on 3 May"})).await.expect("settled");
    let settled = t.get("PaymentException", other).await;
    assert_eq!((&settled["status"], settled["amount"].is_null()), (&json!("Resolved"), true));
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
    // From the request's page: the dialog asks for what Mark Paid reads, its currency the request's
    // own currency record.
    let asks = |name: &str| -> Vec<String> {
        let actions = &t.ctx.schema.entity("PaymentRequest").unwrap().def.actions;
        actions.iter().find(|a| a.name == name).unwrap().params.iter().map(|p| p.name.clone()).collect()
    };
    assert_eq!(asks("mark_paid"), ["amount", "currency", "gateway_reference"]);
    assert_eq!(asks("mark_failed"), ["reason", "gateway_reference"]);
    assert_eq!(asks("set_gateway_details"), ["payment_url", "gateway_reference"]);
    let cod_currency = t.get("PaymentRequest", pr_cod).await["currency"].clone();
    t.k.run_action(&t.ctx, "PaymentRequest", pr_cod, "mark_paid", json!({"amount": retry["payment_request"]["amount"], "currency": cod_currency}))
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

// ───────────────────────────── shipping, tracking, returns and refunds ─────────────────────────────

#[test]
fn returns_and_refunds_end_to_end() {
    let _db = DB.lock().unwrap_or_else(|e| e.into_inner());
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().thread_stack_size(16 << 20).build().unwrap();
    rt.block_on(async { tokio::spawn(refunds()).await.unwrap() });
}

/// What a customer's account owes, from the open item behind a document.
async fn outstanding(t: &T, item: Uuid) -> Decimal {
    let mut tx = t.k.db.tenant_tx(t.ctx.tenant).await.unwrap();
    let v: Option<Decimal> = sqlx::query_scalar("select outstanding from open_item where tenant_id = $1 and id = $2")
        .bind(t.ctx.tenant)
        .bind(item)
        .fetch_optional(&mut *tx)
        .await
        .unwrap();
    v.expect("an open item")
}

/// The subjects and bodies of the mails queued to one address, oldest first.
async fn mails(t: &T, to: &str) -> Vec<(String, String)> {
    let all = events(t, "ferrum.mail").await;
    all.iter()
        .filter(|m| m["to"] == json!(to))
        .map(|m| (m["subject"].as_str().unwrap_or_default().to_string(), m["html"].as_str().unwrap_or_default().to_string()))
        .collect()
}

/// The events a topic put in the outbox, oldest first.
async fn events(t: &T, topic: &str) -> Vec<Value> {
    let mut tx = t.k.db.tenant_tx(t.ctx.tenant).await.unwrap();
    sqlx::query_scalar("select payload from outbox where tenant_id = $1 and topic = $2 order by id")
        .bind(t.ctx.tenant)
        .bind(topic)
        .fetch_all(&mut *tx)
        .await
        .unwrap()
}

struct Placed {
    so: Uuid,
    number: String,
    token: String,
    request: Option<Uuid>,
    total: Decimal,
}

/// A gateway (or a person) confirms a payment request.
async fn pay(t: &T, who: &Ctx, request: Uuid, total: Decimal, reference: &str) {
    t.k.run_action(who, "PaymentRequest", request, "mark_paid", json!({"amount": total.to_string(), "currency": "INR", "gateway_reference": reference}))
        .await
        .expect("paid");
}

/// A guest's order through the storefront, as a browser would place it.
async fn place(t: &T, items: Value, email: &str, method: &str, key: &str) -> Placed {
    let (cart, _) = t.new_cart(None, items).await;
    t.cart_ok(&cart, "PUT", "/cart/details", Some(details(email, "Asha Rao", "IN"))).await;
    let done = t.checkout(None, &cart, key, json!({"payment_method": method, "accept_terms": true})).await.expect("checkout");
    let number = done["order"]["number"].as_str().unwrap().to_string();
    let so = uid(&t.find_one("SalesOrder", json!({"number": number})).await);
    Placed {
        so,
        number,
        token: done["order_token"].as_str().unwrap().to_string(),
        request: done["payment_request"]["id"].as_str().map(|r| r.parse().unwrap()),
        total: dec(&done["order"]["totals"]["rounded_total"]),
    }
}

/// A small shop on a fresh database: mugs and tees at 18% VAT, a flat delivery charge of 50, a
/// card gateway ("fakepay") and cash on delivery.
struct Shop {
    t: T,
    tenant: Uuid,
    cid: Uuid,
    stores: Doc,
    products: Doc,
    nos: Doc,
    selling: Doc,
    mug: Uuid,
    tee: Uuid,
}

async fn small_shop(slug: &str) -> Shop {
    shop_with(slug, &["commerce"]).await
}

async fn shop_with(slug: &str, packages: &[&str]) -> Shop {
    let k = fresh_kernel().await;
    let tenant = k
        .create_tenant(NewTenant {
            slug: slug.into(),
            name: "Refund Shop".into(),
            admin_email: "admin@shop.test".into(),
            admin_password: "Correct-Horse-9".into(),
            admin_name: None,
            packages: packages.iter().map(|p| p.to_string()).collect(),
            settings: Some(json!({"fiscal_year_start_month": 4})),
        })
        .await
        .expect("tenant");
    let session = k.login(slug, "admin@shop.test", "Correct-Horse-9").await.expect("login");
    let ctx = k.load_ctx(tenant, session.user, Some(session.token_version), "it".into()).await.expect("ctx");
    let site = k.hooks.site("commerce").cloned().expect("storefront registered");
    let t = T { k: k.clone(), ctx, site };

    let inr = t.find_one("Currency", json!({"code": "INR"})).await;
    let company = t.insert("Company", json!({"company_name": "Refund Shop Pvt Ltd", "abbr": "RS", "default_currency": inr["id"]})).await;
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
    let tee = uid(&t.insert("Item", item("TEE", "Logo Tee", json!({}))).await);
    let ship = uid(&t.insert("Item", item("SHIP", "Shipping", json!({"is_stock_item": false, "item_group": services["id"]}))).await);
    let selling = t.find_one("PriceList", json!({"price_list_name": "Standard Selling"})).await;
    for (it, rate) in [(mug, "250"), (tee, "400")] {
        t.insert("ItemPrice", json!({"item": it.to_string(), "price_list": selling["id"], "rate": rate})).await;
    }
    let opening = t
        .insert(
            "StockEntry",
            json!({"company": cid.to_string(), "purpose": "Opening Stock", "items": [
                {"item": mug.to_string(), "qty": "10", "basic_rate": "90", "target_warehouse": stores["id"]},
                {"item": tee.to_string(), "qty": "5", "basic_rate": "150", "target_warehouse": stores["id"]}]}),
        )
        .await;
    t.submit("StockEntry", uid(&opening)).await;
    t.insert(
        "StoreSettings",
        json!({"store_name": "Refund Shop", "company": cid.to_string(), "currency": inr["id"], "price_list": selling["id"], "tax_template": sid(&vat),
               "warehouses": [{"warehouse": stores["id"]}], "fulfilment_warehouse": stores["id"], "enforce_stock": true, "show_stock_quantity": true,
               "allow_guest_checkout": true, "shipping_item": ship.to_string(), "allowed_countries": [{"country_code": "IN", "country_name": "India"}],
               "payment_methods": [{"gateway": "fakepay", "label": "Card", "expiry_minutes": 30},
                                   {"gateway": "Cash on Delivery", "label": "Cash on delivery", "expiry_minutes": 0}],
               "store_url": "https://shop.example.com"}),
    )
    .await;
    t.insert("ShippingRule", json!({"label": "Standard", "basis": "Fixed", "amount": "50", "countries": [{"country_code": "IN"}]})).await;
    t.insert("WebsiteItem", json!({"item": mug.to_string(), "published": true})).await;
    t.insert("WebsiteItem", json!({"item": tee.to_string(), "published": true})).await;

    Shop { t, tenant, cid, stores, products, nos, selling, mug, tee }
}

/// A tax rule of the kind country add-ons are: it reads where the goods go from the document's
/// `ship_to` Address (`ferrum_app_erp::localization::ship_to`), as the add-ons for other countries
/// do, and taxes one region at the reduced template. Test-only, and confined to one tenant so the
/// other tests in this binary are not touched.
struct RegionTax(Mutex<Option<Uuid>>, Uuid);

#[async_trait::async_trait]
impl ferrum_app_erp::localization::Localization for RegionTax {
    fn package(&self) -> &'static str {
        "commerce"
    }
    async fn determine(
        &self,
        k: &Kernel,
        ctx: &Ctx,
        tx: &mut ferrum_kernel::Tx,
        _ent: &ferrum_meta::EffEntity,
        doc: &mut Doc,
        _side: ferrum_app_erp::pricing::Side,
    ) -> ferrum_kernel::Result<()> {
        if *self.0.lock().unwrap() != Some(ctx.tenant) {
            return Ok(());
        }
        let place = ferrum_app_erp::localization::ship_to(k, ctx, tx, doc).await?;
        if place.is_some_and(|a| a["state"] == json!("Maharashtra")) {
            doc.insert("tax_template".into(), json!(self.1.to_string()));
        }
        Ok(())
    }
}

#[test]
fn delivery_region_end_to_end() {
    let _db = DB.lock().unwrap_or_else(|e| e.into_inner());
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().thread_stack_size(16 << 20).build().unwrap();
    rt.block_on(async { tokio::spawn(delivery_region()).await.unwrap() });
}

/// Tax rules read where the goods go from the order's `ship_to` Address: each order carries the
/// shopper's own, one place is one Address however often it is used, and the cart is priced for
/// the same region as its order (checkout refuses when their totals differ, so a cart that did not
/// see the region would fail every checkout for a region taxed differently).
async fn delivery_region() {
    let Shop { t, tenant, cid, .. } = shop_with("delivery-region", &["commerce", "crm"]).await;
    let output_tax = t.find_one("Account", json!({"account_name": "Output Tax", "company": cid.to_string()})).await;
    let reduced = t
        .insert(
            "TaxTemplate",
            json!({"title": "VAT 5%", "company": cid.to_string(), "kind": "Sales",
                   "rows": [{"charge_type": "On Net Total", "account": output_tax["id"], "description": "VAT", "rate": "5"}]}),
        )
        .await;
    ferrum_app_erp::localization::add(Arc::new(RegionTax(Mutex::new(Some(tenant)), uid(&reduced))));
    let user = {
        let u = t.k.create_user(&t.ctx, "regular@example.com", "Regular", Some("Correct-Horse-9"), &[], false).await.unwrap();
        t.k.load_ctx(tenant, u, None, "portal".into()).await.unwrap()
    };
    let deliver = |who: Option<&Ctx>, email: &'static str, line1: &'static str, state: &'static str, key: &'static str| {
        let (t, who) = (&t, who.cloned());
        async move {
            let (cart, _) = t.new_cart(who.as_ref(), json!([{"item": "blue-mug", "qty": 1}])).await;
            let mut d = details(email, "Asha Rao", "IN");
            d["shipping_address"]["line1"] = json!(line1);
            d["shipping_address"]["state"] = json!(state);
            let shown = t.cart_ok(&cart, "PUT", "/cart/details", Some(d)).await;
            let done = t.checkout(who.as_ref(), &cart, key, json!({"payment_method": "Cash on Delivery", "accept_terms": true})).await.expect("checkout");
            let so = t.find_one("SalesOrder", json!({"number": done["order"]["number"]})).await;
            assert_eq!(dec(&shown["totals"]["grand_total"]), dec(&so["rounded_total"]), "the total shown as soon as the place was given is the total charged");
            (t.find_one("Address", json!({"id": so["ship_to"]})).await, dec(&so["rounded_total"]), so["customer"].as_str().unwrap().to_string())
        }
    };
    let (home, home_total, _) = deliver(None, "home@example.com", "12 MG Road", "Karnataka", "region-a-0001").await;
    let (away, away_total, _) = deliver(None, "away@example.com", "12 MG Road", "Maharashtra", "region-b-0001").await;
    assert_eq!((home["country_code"].clone(), home["state"].clone()), (json!("IN"), json!("Karnataka")));
    assert_eq!(away["state"], json!("Maharashtra"), "the other order goes elsewhere");
    assert_eq!((home_total, away_total), (n("354"), n("315")), "300 at 18%, and at 5% where the rule says: both checkouts agreed with their carts");

    // One place is one Address of the customer, however many orders go there; a new place adds one.
    let (first, _, customer) = deliver(Some(&user), "regular@example.com", "7 Park Street", "Karnataka", "region-c-0001").await;
    let shipping = || t.count("Address", json!({"customer": customer, "address_type": "Shipping"}));
    assert_eq!(shipping().await, 1);
    let (again, _, same) = deliver(Some(&user), "regular@example.com", " 7  PARK street ", "karnataka", "region-c-0002").await;
    assert_eq!((same, again["id"].clone()), (customer.clone(), first["id"].clone()));
    assert_eq!(shipping().await, 1, "the same place, said a little differently");
    let (other, _, _) = deliver(Some(&user), "regular@example.com", "9 Lake Road", "Maharashtra", "region-c-0003").await;
    assert_ne!(other["id"], first["id"]);
    assert_eq!(shipping().await, 2);
}

#[test]
fn promotions_end_to_end() {
    let _db = DB.lock().unwrap_or_else(|e| e.into_inner());
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().thread_stack_size(16 << 20).build().unwrap();
    rt.block_on(async { tokio::spawn(promotions()).await.unwrap() });
}

/// The cart and its order are priced by ERP's promotions, as any sales order is: an automatic amount
/// off the order, free delivery and an amount off one item by codes, several codes at once, a
/// promotion of another channel left out; a promotion that changes between cart and checkout is
/// caught by the total check and the order then agrees with the cart; a code is counted once.
async fn promotions() {
    let Shop { t, cid, .. } = small_shop("promotions").await;
    let ship = t.find_one("Item", json!({"item_code": "SHIP"})).await;
    let ship = t.get("Item", uid(&ship)).await;
    assert_eq!(ship["is_delivery_charge"], json!(true), "the store's shipping item is a delivery charge");
    let tee = t.find_one("Item", json!({"item_code": "TEE"})).await;
    let off = t
        .insert(
            "Promotion",
            json!({"title": "100 off 500", "company": cid.to_string(), "channels": "Web Shop", "gives": "Amount Off the Document", "amount": "100", "min_amount": "500"}),
        )
        .await;
    let free = t.insert("Promotion", json!({"title": "Free delivery", "requires_code": true, "gives": "Free Delivery"})).await;
    t.insert("PromotionCode", json!({"code": "SHIPFREE", "promotion": sid(&free), "max_uses": 1})).await;
    let tee50 = t
        .insert(
            "Promotion",
            json!({"title": "50 off the tee", "requires_code": true, "priority": 5, "gives": "Amount Off Qualifying Lines", "amount": "50",
                   "qualifying_items": [{"item": sid(&tee)}]}),
        )
        .await;
    t.insert("PromotionCode", json!({"code": "TEE50", "promotion": sid(&tee50)})).await;
    t.insert("Promotion", json!({"title": "Till only", "channels": "Point of Sale", "gives": "Percent Off the Document", "percent": "50"})).await;

    let (cart, view) = t.new_cart(None, json!([{"item": "logo-tee", "qty": 1}])).await;
    assert_eq!(dec(&view["totals"]["discount"]), n("0"), "400 is under the minimum spend");
    t.cart_ok(&cart, "POST", "/cart/items", Some(json!({"item": "blue-mug", "qty": 1}))).await;
    let view = t.cart_ok(&cart, "PUT", "/cart/details", Some(details(GUEST, "Asha Rao", "IN"))).await;
    let totals = &view["totals"];
    assert_eq!(
        (dec(&totals["discount"]), dec(&totals["shipping"]), dec(&totals["grand_total"])),
        (n("100"), n("50"), n("708")),
        "(650 - 100 + 50) at 18%: {view}"
    );
    assert_eq!(view["promotions"], json!([{"title": "100 off 500", "amount": "100"}]));

    let view = t.cart_ok(&cart, "PUT", "/cart/coupon", Some(json!({"code": "shipfree"}))).await;
    assert_eq!((dec(&view["totals"]["shipping"]), dec(&view["totals"]["grand_total"])), (n("0"), n("649")), "delivery is free, the rest as it was");
    let view = t.cart_ok(&cart, "PUT", "/cart/coupon", Some(json!({"code": "TEE50"}))).await;
    assert_eq!(dec(&view["totals"]["grand_total"]), n("590"), "two codes at once: (650 - 50 - 100) at 18%");
    assert_eq!(view["codes"], json!([{"code": "SHIPFREE", "applied": true}, {"code": "TEE50", "applied": true}]));
    let bad = t.cart(&cart, "PUT", "/cart/coupon", Some(json!({"code": "NOPE-NOPE"}))).await;
    assert_eq!(field_error(&bad, "coupon_code"), "this coupon code is not valid");

    // The automatic promotion grows after the shopper saw 590: checkout says so, then agrees.
    t.k.update(&t.ctx, "Promotion", uid(&off), obj(json!({"amount": "150"})), None).await.unwrap();
    let stale = t.checkout(None, &cart, "promo-order-01", json!({"payment_method": "Cash on Delivery", "accept_terms": true, "expected_total": "590"})).await;
    assert!(matches!(stale, Err(Error::Conflict(ref m)) if m.contains("531")), "{stale:?}");
    let done = t
        .checkout(None, &cart, "promo-order-02", json!({"payment_method": "Cash on Delivery", "accept_terms": true, "expected_total": "531"}))
        .await
        .expect("checkout");
    let so = t.find_one("SalesOrder", json!({"number": done["order"]["number"]})).await;
    let so = t.get("SalesOrder", uid(&so)).await;
    assert_eq!((dec(&so["rounded_total"]), so["channel"].clone()), (n("531"), json!("Web Shop")), "the order is priced as its cart");
    let given: Decimal = so["applied_promotions"].as_array().unwrap().iter().map(|a| dec(&a["amount"])).sum();
    assert_eq!(given, n("250"), "150 off the order, 50 off the tee, 50 of delivery");
    let code = t.find_one("PromotionCode", json!({"code": "SHIPFREE"})).await;
    assert_eq!(code["used_count"], json!(1), "counted once, when the order was submitted");

    // Its one use is gone: another cart cannot take it.
    let (other, _) = t.new_cart(None, json!([{"item": "logo-tee", "qty": 1}])).await;
    let used = t.cart(&other, "PUT", "/cart/coupon", Some(json!({"code": "SHIPFREE"}))).await;
    assert_eq!(field_error(&used, "coupon_code"), "this coupon code is not valid", "used up and unknown sound the same");
    let view = t.cart_ok(&other, "PUT", "/cart/coupon", Some(json!({"code": "TEE50"}))).await;
    let view2 = t.cart_ok(&other, "DELETE", "/cart/codes/tee50", None).await;
    assert!(dec(&view["totals"]["grand_total"]) < dec(&view2["totals"]["grand_total"]) && view2["codes"] == json!([]), "a code comes off again");
}

#[test]
fn gift_cards_end_to_end() {
    let _db = DB.lock().unwrap_or_else(|e| e.into_inner());
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().thread_stack_size(16 << 20).build().unwrap();
    rt.block_on(async { tokio::spawn(gift_cards()).await.unwrap() });
}

/// A gift card is sold (its code reaches the buyer once, in a message, and is kept nowhere in the
/// clear), pays most of another order with the rest by card, and a return of that order goes back
/// onto it. A wrong code leaves nothing behind.
async fn gift_cards() {
    let Shop { t, cid, products, nos, selling, .. } = small_shop("gift-cards").await;
    let c = cid.to_string();
    let current = t.find_one("Account", json!({"account_name": "Current Liabilities", "company": c})).await;
    let liability = t.insert("Account", json!({"account_name": "Gift Cards", "company": c, "parent_account": current["id"], "root_type": "Liability"})).await;
    let goodwill = t.find_one("Account", json!({"account_name": "Administrative Expenses", "company": c})).await;
    let kind = t
        .insert("StoredValueKind", json!({"kind_name": "Gift Card", "company": c, "liability_account": liability["id"], "adjustment_account": goodwill["id"]}))
        .await;
    let settings = t.find_one("StoreSettings", json!({})).await;
    t.k.update(&t.ctx, "StoreSettings", uid(&settings), obj(json!({"gift_card_kind": sid(&kind)})), None).await.unwrap();
    let card_item = t
        .insert(
            "Item",
            json!({"item_code": "GIFT500", "item_name": "Gift Card 500", "item_group": products["id"], "stock_uom": nos["id"], "is_stock_item": false,
                   "stored_value_kind": sid(&kind)}),
        )
        .await;
    t.insert("ItemPrice", json!({"item": sid(&card_item), "price_list": selling["id"], "rate": "500"})).await;
    t.insert("WebsiteItem", json!({"item": sid(&card_item), "published": true})).await;

    // ───────────── bought: the code goes to the buyer once ─────────────
    let bought = place(&t, json!([{"item": "gift-card-500", "qty": 1}]), "giver@example.com", "fakepay", "gift-buy-0001").await;
    assert_eq!(bought.total, n("649"), "(500 + 50 of delivery) at 18%: taxed when sold");
    pay(&t, &t.ctx, bought.request.unwrap(), bought.total, "fp_gift_1").await;
    pay(&t, &t.ctx, bought.request.unwrap(), bought.total, "fp_gift_1").await;
    let sent: Vec<_> = opened_mails(&t, "giver@example.com").await.into_iter().filter(|(s, _)| s.contains("gift card")).collect();
    assert_eq!(sent.len(), 1, "told once, a repeated payment notice changes nothing: {sent:?}");
    let body = &sent[0].1;
    let code = body.split("<strong>").nth(1).and_then(|r| r.split("</strong>").next()).unwrap().to_string();
    assert!(code.len() >= 8 && body.contains("500"), "{body}");
    // While it waits for the mailer the outbox holds the code sealed, and only a placeholder where it goes.
    let waiting = mails(&t, "giver@example.com").await.into_iter().filter(|(s, _)| s.contains("gift card")).collect::<Vec<_>>();
    assert!(waiting.len() == 1 && !waiting[0].1.contains(&code) && waiting[0].1.contains("••••••"), "the code is not in the clear in the outbox: {waiting:?}");
    let account = t.find_one("StoredValueAccount", json!({"kind": sid(&kind)})).await;
    assert_eq!(dec(&account["balance"]), n("500"));
    assert_ne!(account["code_hash"], json!(code), "kept as a hash");
    let so = t.get("SalesOrder", bought.so).await;
    assert!(!so.values().any(|v| v.to_string().contains(&code)) && so["commerce_cards_sent"] == json!(true), "the order keeps that it went, not the code");

    // ───────────── a wrong code: nothing is made ─────────────
    let orders_before = t.count("SalesOrder", json!({})).await;
    let (cart, _) = t.new_cart(None, json!([{"item": "logo-tee", "qty": 1}])).await;
    t.cart_ok(&cart, "PUT", "/cart/details", Some(details("taker@example.com", "Ravi Rao", "IN"))).await;
    let wrong =
        t.checkout(None, &cart, "gift-use-0001", json!({"payment_method": "fakepay", "accept_terms": true, "gift_card": {"code": "NOT-A-CARD-0000"}})).await;
    assert!(field_error(&wrong, "gift_card").contains("no stored value account"), "{wrong:?}");
    assert_eq!(t.count("SalesOrder", json!({})).await, orders_before, "the order went with the refusal");

    // ───────────── used: 500 from the card, the rest by card ─────────────
    let done = t
        .checkout(None, &cart, "gift-use-0002", json!({"payment_method": "fakepay", "accept_terms": true, "gift_card": {"code": code.to_lowercase()}}))
        .await
        .expect("checkout");
    let total = dec(&done["order"]["totals"]["rounded_total"]);
    assert_eq!(total, n("531"), "(400 + 50) at 18%");
    assert_eq!((dec(&done["payment_request"]["amount"]), done["order"]["payment_status"].clone()), (n("31"), json!("Pending")), "the card took 500");
    // Held for the order, not taken: nothing is invoiced before the rest is paid.
    let held = t.get("StoredValueAccount", uid(&account)).await;
    assert_eq!((dec(&held["balance"]), dec(&held["held"])), (n("500"), n("500")), "the card's value is set aside for the order");
    let so_id = uid(&t.find_one("SalesOrder", json!({"number": done["order"]["number"]})).await);
    assert_eq!(t.count("SalesInvoice", json!({"items.sales_order": so_id.to_string()})).await, 0, "no invoice before it is fully paid");
    let so = t.get("SalesOrder", so_id).await;
    assert_eq!((dec(&so["commerce_card_held"]), dec(&so["commerce_paid_by_card"])), (n("500"), n("0")));
    let rest_request: Uuid = done["payment_request"]["id"].as_str().unwrap().parse().unwrap();
    assert!(t.get("PaymentRequest", rest_request).await["expires_at"].is_string(), "the rest is asked for within the hold");
    pay(&t, &t.ctx, rest_request, n("31"), "fp_rest_1").await;
    let account_now = t.get("StoredValueAccount", uid(&account)).await;
    assert_eq!((dec(&account_now["balance"]), dec(&account_now["held"])), (n("0"), n("0")), "paid: the invoice took the card's value");
    let so = t.get("SalesOrder", so_id).await;
    assert_eq!((so["commerce_payment_status"].clone(), dec(&so["commerce_paid_by_card"])), (json!("Paid"), n("500")));
    let invoice = t.find_one("SalesInvoice", json!({"items.sales_order": so_id.to_string(), "is_return": false})).await;
    assert_eq!(dec(&invoice["outstanding_amount"]), n("0"), "the card and the card payment settle the invoice made at checkout");

    // ───────────── shipped, then the tee comes back: onto the card ─────────────
    t.k.run_action(&t.ctx, "SalesOrder", so_id, "web_ship", json!({"notify": "No"})).await.expect("shipped, with nothing left to invoice");
    let so = t.get("SalesOrder", so_id).await;
    let tee_line = so["items"].as_array().unwrap().iter().find(|l| l["item_name"] == json!("Logo Tee")).unwrap()["id"].as_str().unwrap().to_string();
    let reason = t.find_one("ReturnReason", json!({"reason": "Changed their mind"})).await;
    let refund = t
        .insert(
            "OrderRefund",
            json!({"sales_order": so_id.to_string(), "kind": "Return", "reason": reason["id"], "lines": [{"sales_order_line": tee_line, "qty": "1"}]}),
        )
        .await;
    assert_eq!((refund["gateway"].clone(), dec(&refund["amount"])), (json!("Gift Card"), n("472")), "the card is paid back first: 400 at 18%");
    t.k.run_action(&t.ctx, "OrderRefund", uid(&refund), "approve_refund", json!({})).await.expect("approved");
    let refund = t.get("OrderRefund", uid(&refund)).await;
    assert_eq!((refund["status"].clone(), refund["booked"].clone()), (json!("Refunded"), json!(true)), "{refund:?}");
    assert_eq!(dec(&t.get("StoredValueAccount", uid(&account)).await["balance"]), n("472"), "back onto the card that paid");

    let fakepay = gateway(&t, "fakepay").await;
    let with_card = |item: &'static str, email: &'static str, key: &'static str, method: Option<&'static str>| {
        let (t, code) = (&t, code.clone());
        async move {
            let (cart, _) = t.new_cart(None, json!([{"item": item, "qty": 1}])).await;
            t.cart_ok(&cart, "PUT", "/cart/details", Some(details(email, "Card Holder", "IN"))).await;
            let mut body = json!({"accept_terms": true, "gift_card": {"code": code}});
            if let Some(m) = method {
                body["payment_method"] = json!(m);
            }
            let done = t.checkout(None, &cart, key, body).await.expect("checkout");
            let so = uid(&t.find_one("SalesOrder", json!({"number": done["order"]["number"]})).await);
            (so, done["payment_request"]["id"].as_str().and_then(|r| r.parse::<Uuid>().ok()))
        }
    };
    let card = || async { t.get("StoredValueAccount", uid(&account)).await };

    // ───────────── part held, the rest never paid: the card is let go, the order cancels ─────────────
    let (late, request) = with_card("logo-tee", "late@example.com", "gift-late-0001", Some("fakepay")).await;
    assert_eq!((dec(&card().await["held"]), dec(&card().await["balance"])), (n("472"), n("472")));
    t.sql(&format!("update e_payment_request set expires_at = now() - interval '1 minute' where id = '{}'", request.unwrap())).await;
    ferrum_app_commerce::maintenance(&t.k).await.expect("upkeep");
    assert_eq!(t.get("PaymentRequest", request.unwrap()).await["status"], json!("Expired"));
    assert_eq!(t.get("SalesOrder", late).await["docstatus"], json!(2), "cancelled by the upkeep: nothing was invoiced, so nothing stops it");
    assert_eq!((dec(&card().await["held"]), dec(&card().await["balance"])), (n("0"), n("472")), "the card's value is free again");
    assert_eq!(dec(&t.get("SalesOrder", late).await["commerce_card_held"]), n("0"));

    // ───────────── paid two ways, then cancelled: the card gets its value back, the gateway's part goes through it ─────────────
    let (two, request) = with_card("logo-tee", "two@example.com", "gift-two-0001", Some("fakepay")).await;
    pay(&t, &t.ctx, request.unwrap(), n("59"), "fp_two_1").await;
    assert_eq!(dec(&card().await["balance"]), n("0"));
    let cancelled = t.k.run_action(&t.ctx, "SalesOrder", two, "web_cancel", json!({})).await.expect("cancelled");
    assert_eq!(cancelled["refunds"].as_array().unwrap().len(), 2, "a refund for each payment: {cancelled}");
    assert_eq!(cancelled["cancelled"], json!(false), "the gateway's part is still on its way");
    assert_eq!(dec(&card().await["balance"]), n("0"), "the card waits for the part that goes through a provider, which can fail");
    let theirs = t.find_one("OrderRefund", json!({"sales_order": two.to_string(), "gateway": "fakepay"})).await;
    assert_eq!((theirs["status"].clone(), dec(&theirs["amount"])), (json!("Approved"), n("59")));
    t.k.run_action(&fakepay, "OrderRefund", uid(&theirs), "refund_failed", json!({"error": "the bank said later"})).await.expect("failed");
    assert_eq!(t.get("OrderRefund", uid(&theirs)).await["status"], json!("Refund Failed"), "recorded, and a person can try again");
    assert_eq!(t.get("SalesOrder", two).await["docstatus"], json!(1), "the order stands until both have gone back");
    t.k.run_action(&t.ctx, "OrderRefund", uid(&theirs), "approve_refund", json!({})).await.expect("again");
    t.k.run_action(&fakepay, "OrderRefund", uid(&theirs), "refund_done", json!({"refund_reference": "re_two"})).await.expect("done");
    let mine = t.find_one("OrderRefund", json!({"sales_order": two.to_string(), "gateway": "Gift Card"})).await;
    assert_eq!((mine["status"].clone(), mine["booked"].clone(), dec(&mine["amount"])), (json!("Refunded"), json!(true), n("472")), "{mine:?}");
    assert_eq!(dec(&card().await["balance"]), n("472"), "the card has its value back");
    let so = t.get("SalesOrder", two).await;
    assert_eq!((so["docstatus"].clone(), so["commerce_payment_status"].clone()), (json!(2), json!("Refunded")));
    assert_eq!(t.count("SalesInvoice", json!({"items.sales_order": two.to_string(), "docstatus": 1})).await, 0, "its invoice went with it");

    // ───────────── a card alone paid it: cancelled and given back in one go ─────────────
    let (alone, none) = with_card("blue-mug", "alone@example.com", "gift-alone-0001", None).await;
    let _ = none;
    assert_eq!(dec(&card().await["balance"]), n("118"), "354 of 472 taken");
    let cancelled = t.k.run_action(&t.ctx, "SalesOrder", alone, "web_cancel", json!({})).await.expect("cancelled");
    assert_eq!(cancelled["cancelled"], json!(true), "{cancelled}");
    assert_eq!(dec(&card().await["balance"]), n("472"));
    // Cancelled with nothing paid: held value goes with the order.
    let (unpaid, _) = with_card("logo-tee", "unpaid@example.com", "gift-unpaid-0001", Some("fakepay")).await;
    assert_eq!(dec(&card().await["held"]), n("472"));
    let cancelled = t.k.run_action(&t.ctx, "SalesOrder", unpaid, "web_cancel", json!({})).await.expect("cancelled");
    assert_eq!(cancelled["cancelled"], json!(true));
    assert_eq!((dec(&card().await["held"]), dec(&t.get("SalesOrder", unpaid).await["commerce_card_held"])), (n("0"), n("0")));
}

#[test]
fn fulfilment_end_to_end() {
    let _db = DB.lock().unwrap_or_else(|e| e.into_inner());
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().thread_stack_size(16 << 20).build().unwrap();
    rt.block_on(async { tokio::spawn(fulfilment()).await.unwrap() });
}

/// An order ships in parts and from two warehouses: each line is reserved where its stock is (so
/// what the shop showed as available is what ships), the desk ships part of a line now from another
/// warehouse and the rest later, each shipment its own note with its own tracking and message, and
/// the order's fulfilment follows; the shipment that completes it invoices it.
async fn fulfilment() {
    let Shop { t, cid, stores, products, nos, selling, mug, .. } = small_shop("fulfilment").await;
    let c = cid.to_string();
    let annex = t.insert("Warehouse", json!({"warehouse_name": "Annex", "company": c})).await;
    let cap = uid(&t.insert("Item", json!({"item_code": "CAP", "item_name": "Cap", "item_group": products["id"], "stock_uom": nos["id"]})).await);
    t.insert("ItemPrice", json!({"item": cap.to_string(), "price_list": selling["id"], "rate": "150"})).await;
    t.insert("WebsiteItem", json!({"item": cap.to_string(), "published": true})).await;
    let opening = t
        .insert(
            "StockEntry",
            json!({"company": c, "purpose": "Opening Stock", "items": [
                {"item": cap.to_string(), "qty": "4", "basic_rate": "60", "target_warehouse": annex["id"]},
                {"item": mug.to_string(), "qty": "5", "basic_rate": "90", "target_warehouse": annex["id"]}]}),
        )
        .await;
    t.submit("StockEntry", uid(&opening)).await;
    let settings = t.find_one("StoreSettings", json!({})).await;
    t.k.update(&t.ctx, "StoreSettings", uid(&settings), obj(json!({"warehouses": [{"warehouse": stores["id"]}, {"warehouse": annex["id"]}]})), None)
        .await
        .unwrap();
    assert_eq!(t.available("cap").await, n("4"), "the cap is only in the annex, and the shop counts the annex");

    let placed = place(&t, json!([{"item": "blue-mug", "qty": 3}, {"item": "cap", "qty": 2}]), GUEST, "Cash on Delivery", "ship-parts-01").await;
    let so = t.get("SalesOrder", placed.so).await;
    let line = |item: Uuid| so["items"].as_array().unwrap().iter().find(|l| l["item"] == json!(item.to_string())).unwrap().clone();
    let (mug_line, cap_line) = (line(mug), line(cap));
    assert_eq!((mug_line["warehouse"].clone(), cap_line["warehouse"].clone()), (stores["id"].clone(), annex["id"].clone()), "each line where its stock is");
    assert_eq!(so["commerce_fulfilment_status"], json!("Unfulfilled"));

    // Part of the mugs now, from the annex (the desk's choice), with their own tracking.
    let bad = t.k.run_action(&t.ctx, "SalesOrder", placed.so, "web_ship", json!({"lines": [{"line": mug_line["id"], "qty": "9"}]})).await;
    assert!(matches!(&bad, Err(Error::Validation(f)) if f[0].message.contains("at most 3")), "{bad:?}");
    let first = t
        .k
        .run_action(
            &t.ctx,
            "SalesOrder",
            placed.so,
            "web_ship",
            json!({"lines": json!([{"line": mug_line["id"], "qty": "1", "warehouse": annex["id"]}]).to_string(), "carrier": "BlueDart", "tracking_number": "BD1"}),
        )
        .await
        .expect("first shipment");
    assert_eq!((first["fulfilment"].clone(), first["sales_invoice"].clone()), (json!("Partly Fulfilled"), Value::Null), "not invoiced until it is all out");
    let note = t.get("DeliveryNote", first["delivery_note"]["id"].as_str().unwrap().parse().unwrap()).await;
    assert_eq!(note["items"].as_array().unwrap().len(), 1);
    assert_eq!((note["items"][0]["warehouse"].clone(), dec(&note["items"][0]["qty"])), (annex["id"].clone(), n("1")));
    assert_eq!(t.get("SalesOrder", placed.so).await["commerce_fulfilment_status"], json!("Partly Fulfilled"));

    // The rest, each line from where the order reserved it.
    let second = t.k.run_action(&t.ctx, "SalesOrder", placed.so, "web_ship", json!({"tracking_number": "BD2"})).await.expect("second shipment");
    assert_eq!(second["fulfilment"], json!("Fulfilled"));
    assert!(second["sales_invoice"]["id"].is_string(), "the shipment that completes the order invoices it");
    let note = t.get("DeliveryNote", second["delivery_note"]["id"].as_str().unwrap().parse().unwrap()).await;
    let from: Vec<(Value, Decimal)> = note["items"].as_array().unwrap().iter().map(|l| (l["warehouse"].clone(), dec(&l["qty"]))).collect();
    assert!(from.contains(&(stores["id"].clone(), n("2"))) && from.contains(&(annex["id"].clone(), n("2"))), "{from:?}");
    let so = t.get("SalesOrder", placed.so).await;
    assert_eq!(so["commerce_fulfilment_status"], json!("Fulfilled"));

    let shipped: Vec<_> = mails(&t, GUEST).await.into_iter().filter(|(s, _)| s.contains("has shipped")).collect();
    assert_eq!(shipped.len(), 2, "told of each shipment");
    assert!(shipped[0].1.contains("1 × Blue Mug") && shipped[0].1.contains("parcel BD1"), "{shipped:?}");
    assert!(shipped[1].1.contains("Its parcel is BD2") && shipped[1].1.contains("Cap"), "{shipped:?}");
    let order = t.ok(None, "POST", "/orders/lookup", &[], Some(json!({"number": placed.number, "email": GUEST, "token": placed.token}))).await["order"].clone();
    assert_eq!((order["fulfilment"].clone(), order["shipments"].as_array().map(Vec::len)), (json!("Fulfilled"), Some(2)), "{order}");
}

async fn refunds() {
    let Shop { t, tenant, mug, tee, .. } = small_shop("refunds").await;
    // ───────────── who is who ─────────────
    let person = |email: &'static str, roles: Vec<String>| {
        let (k, admin) = (t.k.clone(), t.ctx.clone());
        async move {
            let u = k.create_user(&admin, email, "Someone", Some("Correct-Horse-9"), &roles, false).await.unwrap();
            k.load_ctx(tenant, u, None, "it".into()).await.unwrap()
        }
    };
    let manager = person("manager@shop.test", vec!["Website Manager".into()]).await;
    let clerk = person("clerk@shop.test", vec!["Sales User".into()]).await;
    let stranger = person("stranger@example.com", vec![]).await;
    let gateway_user = |name: &'static str| {
        let (k, admin) = (t.k.clone(), t.ctx.clone());
        async move {
            let role = format!("plugin:{name}");
            for entity in ["PaymentRequest", "PaymentException", "OrderRefund"] {
                let p = PermissionDef { entity: entity.into(), read: true, write: true, ..Default::default() };
                k.set_role_permission(&admin, &role, &p).await.unwrap();
            }
            let u = k.create_user(&admin, &format!("{name}@plugins.ferrum.invalid"), &format!("Plugin: {name}"), None, &[role], false).await.unwrap();
            k.load_ctx(tenant, u, None, format!("plugin-{name}")).await.unwrap()
        }
    };
    let fakepay = gateway_user("fakepay").await;
    let otherpay = gateway_user("otherpay").await;

    // ───────────── an order is paid, and the store's manager ships it from the desk ─────────────
    // A first order is abandoned: cash on delivery, cancelled before anything happened.
    let a = place(&t, json!([{"item": "blue-mug", "qty": 1}]), "kiran@example.com", "Cash on Delivery", "refund-a-0001").await;
    t.k.cancel(&t.ctx, "SalesOrder", a.so, None).await.expect("cancelled");
    assert_eq!(t.available("blue-mug").await, n("10"), "its mug is free again");
    let seen_a = t.ok(None, "POST", "/orders/track", &[], Some(json!({"number": a.number, "token": a.token}))).await;
    assert_eq!(seen_a["order"]["status"], json!("Cancelled"), "a cancelled order can still be followed");

    let b = place(&t, json!([{"item": "blue-mug", "qty": 2}, {"item": "logo-tee", "qty": 1}]), GUEST, "fakepay", "refund-b-0001").await;
    assert_eq!(b.total, n("1121"), "950 of goods and delivery, 171 of VAT");
    let thanked = mails(&t, GUEST).await;
    assert!(
        thanked.iter().any(|(subject, html)| subject.starts_with(&format!("Your order {}", b.number)) && html.contains(&format!("n={}", b.number))),
        "the shopper is told of the order, with its link: {thanked:?}"
    );
    assert_eq!(t.get("SalesOrder", b.so).await["commerce_payment_method"], json!("fakepay"), "the order remembers how it is paid");
    pay(&t, &fakepay, b.request.unwrap(), b.total, "pi_b").await;
    assert_eq!(t.get("SalesOrder", b.so).await["commerce_payment_status"], json!("Paid"));
    let refused = t.k.run_action(&stranger, "SalesOrder", b.so, "web_ship", json!({})).await;
    assert!(matches!(refused, Err(Error::Forbidden(_))), "only the store's people ship: {refused:?}");
    let bad_link = t.k.run_action(&manager, "SalesOrder", b.so, "web_ship", json!({"tracking_url": "http://insecure.example/x"})).await;
    assert!(matches!(bad_link, Err(Error::Validation(_))), "tracking links are https: {bad_link:?}");
    let shipped =
        t.k.run_action(
            &manager,
            "SalesOrder",
            b.so,
            "web_ship",
            json!({"carrier": "BlueDart", "tracking_number": "BD123", "tracking_url": "https://track.example.com/BD123", "notify": "Yes", "invoice": "Yes"}),
        )
        .await
        .expect("a Website Manager ships and invoices without holding the Sales roles");
    let dn = shipped["delivery_note"]["id"].as_str().unwrap().parse::<Uuid>().unwrap();
    let dn_doc = t.get("DeliveryNote", dn).await;
    assert_eq!(
        (dn_doc["docstatus"].clone(), dn_doc["commerce_tracking_number"].clone(), dn_doc["transporter"].clone()),
        (json!(1), json!("BD123"), json!("BlueDart"))
    );
    let invoice = shipped["sales_invoice"]["id"].as_str().unwrap().parse::<Uuid>().unwrap();
    let si = t.get("SalesInvoice", invoice).await;
    assert_eq!((si["docstatus"].clone(), dec(&si["outstanding_amount"])), (json!(1), Decimal::ZERO), "the web payment is applied to the invoice");
    assert_eq!(dec(&t.get("SalesOrder", b.so).await["per_delivered"]), n("100"));
    let again = t.k.run_action(&manager, "SalesOrder", b.so, "web_ship", json!({})).await;
    assert!(again.is_err(), "nothing is left to ship: {again:?}");
    let told = events(&t, "commerce.order_shipped").await;
    let mine = told.iter().find(|e| e["number"] == json!(b.number)).expect("the shipment is announced");
    assert_eq!((mine["tracking_number"].clone(), mine["notify"].clone()), (json!("BD123"), json!(true)));
    assert_eq!(mine["order_link"], json!(format!("https://shop.example.com/order#n={}&t={}", b.number, b.token)), "the link a mailer sends");
    let sent = mails(&t, GUEST).await;
    let shipped_mail = sent.iter().find(|(subject, _)| *subject == format!("Your order {} has shipped", b.number)).expect("the shopper is told it has shipped");
    assert!(shipped_mail.1.contains("BlueDart has it, parcel BD123") && shipped_mail.1.contains(&format!("t={}", b.token)), "{shipped_mail:?}");

    // ───────────── a shopper follows the order from a link, with no sign-in ─────────────
    let tracked = t.ok(None, "POST", "/orders/track", &[], Some(json!({"number": b.number, "token": b.token}))).await;
    let order = &tracked["order"];
    assert_eq!(order["shipments"][0]["tracking_number"], json!("BD123"));
    assert_eq!(order["shipments"][0]["tracking_url"], json!("https://track.example.com/BD123"));
    assert_eq!(order["steps"].as_array().unwrap().iter().map(|s| s["done"].as_bool().unwrap()).collect::<Vec<_>>(), vec![true, true, true]);
    let raw = tracked.to_string();
    for private in [GUEST, "MG Road", "Flat 4", "Asha Rao"] {
        assert!(!raw.contains(private), "the link holder is not shown {private}: {raw}");
    }
    assert_eq!(order["ship_to"], json!("Bengaluru, Karnataka 560001 · IN"));
    for (number, token) in [(b.number.as_str(), "AAAA"), ("SO-NOPE", b.token.as_str()), (b.number.as_str(), a.token.as_str())] {
        let miss = t.call(None, "POST", "/orders/track", &[], Some(json!({"number": number, "token": token}))).await;
        assert!(matches!(miss, Err(Error::NotFound(_))), "every wrong link looks the same: {miss:?}");
    }
    let link = t.k.run_action(&manager, "SalesOrder", b.so, "web_tracking_link", json!({})).await.unwrap();
    assert_eq!(link["link"], json!(format!("https://shop.example.com/order#n={}&t={}", b.number, b.token)), "the desk can copy it too");

    // ───────────── a return: one mug and the delivery charge ─────────────
    let so_b = t.get("SalesOrder", b.so).await;
    let line =
        |item: Uuid| so_b["items"].as_array().unwrap().iter().find(|l| l["item"] == json!(item.to_string())).unwrap()["id"].as_str().unwrap().to_string();
    let (mug_line, tee_line) = (line(mug), line(tee));
    // The desk asks what may come back before it asks for anything, and nothing is left behind.
    let plan = |body: Value| t.call(Some(&manager), "POST", "/desk/refund-plan", &[], Some(body));
    let plan0 = plan(json!({"sales_order": b.so.to_string()})).await.unwrap();
    assert_eq!(
        (plan0["payable"].clone(), plan0["payment"]["gateway"].clone(), plan0["payment"]["label"].clone()),
        (json!(true), json!("fakepay"), json!("Card"))
    );
    assert_eq!((dec(&plan0["payment"]["paid"]), dec(&plan0["payment"]["refundable"])), (n("1121"), n("1121")));
    assert_eq!(plan0["lines"].as_array().unwrap().len(), 2, "the delivery charge is not a line to send back");
    assert!(plan0["lines"].as_array().unwrap().iter().all(|l| l["invoiced"] == json!(true)));
    assert_eq!(dec(&plan0["lines"].as_array().unwrap().iter().find(|l| l["id"] == json!(mug_line)).unwrap()["returnable"]), n("2"));
    assert_eq!(
        (plan0["shipping"]["available"].clone(), plan0["cancel"]["allowed"].clone(), plan0["estimate"].clone()),
        (json!(true), json!(false), Value::Null),
        "it has shipped"
    );
    let plan1 = plan(json!({"sales_order": b.so.to_string(), "lines": [{"sales_order_line": mug_line, "qty": "1"}], "include_shipping": true})).await.unwrap();
    assert_eq!((dec(&plan1["estimate"]["amount"]), plan1["estimate"]["errors"].clone()), (n("354"), json!([])));
    let plan2 = plan(json!({"sales_order": b.so.to_string(), "lines": [{"sales_order_line": mug_line, "qty": "3"}]})).await.unwrap();
    assert!(plan2["estimate"]["errors"][0].as_str().unwrap().contains("at most 2 of Blue Mug"), "{plan2}");
    assert_eq!(t.count("SalesInvoice", json!({"is_return": true})).await, 0, "planning leaves no credit note behind");
    let nobody = t.call(Some(&stranger), "POST", "/desk/refund-plan", &[], Some(json!({"sales_order": b.so.to_string()}))).await;
    assert!(matches!(nobody, Err(Error::Forbidden(_))), "only the store's people plan refunds: {nobody:?}");
    assert!(matches!(t.call(None, "POST", "/desk/refund-plan", &[], Some(json!({"sales_order": b.so.to_string()}))).await, Err(Error::Unauthorized(_))));
    let reason = t.find_one("ReturnReason", json!({"reason": "Changed their mind"})).await;
    let ask = |lines: Value, shipping: bool| json!({"sales_order": b.so.to_string(), "kind": "Return", "reason": reason["id"], "lines": lines, "include_shipping": shipping});
    let over = t.k.insert(&manager, "OrderRefund", obj(ask(json!([{"sales_order_line": mug_line, "qty": "3"}]), false))).await;
    assert!(matches!(&over, Err(Error::Validation(f)) if f.iter().any(|x| x.message.contains("at most 2 of Blue Mug"))), "{over:?}");
    let strange = t.k.insert(&manager, "OrderRefund", obj(ask(json!([{"sales_order_line": Uuid::new_v4().to_string(), "qty": "1"}]), false))).await;
    assert!(matches!(&strange, Err(Error::Validation(_))), "a line that is not the order's: {strange:?}");
    let nothing = t.k.insert(&manager, "OrderRefund", obj(ask(json!([]), true))).await;
    assert!(nothing.is_err(), "choose what comes back: {nothing:?}");
    let twice =
        t.k.insert(&manager, "OrderRefund", obj(ask(json!([{"sales_order_line": mug_line, "qty": "1"}, {"sales_order_line": mug_line, "qty": "1"}]), false)))
            .await;
    assert!(matches!(&twice, Err(Error::Validation(_))), "the same line twice: {twice:?}");
    let mut sneaky = ask(json!([{"sales_order_line": mug_line, "qty": "1"}]), false);
    sneaky["amount"] = json!("1");
    match t.k.insert(&manager, "OrderRefund", obj(sneaky)).await {
        Err(Error::Validation(_)) => {}
        Ok(made) => {
            assert_eq!(dec(&made["amount"]), n("295"), "an amount from the caller is not taken");
            t.k.run_action(&manager, "OrderRefund", uid(&made), "decline", json!({})).await.unwrap();
        }
        other => panic!("{other:?}"),
    }
    let r1 = t.k.insert(&manager, "OrderRefund", obj(ask(json!([{"sales_order_line": mug_line, "qty": "1", "restock": true}]), true))).await;
    let r1 = r1.unwrap_or_else(|e| panic!("a return is asked for: {e:?}"));
    let r1id = uid(&r1);
    assert_eq!(dec(&r1["amount"]), n("354"), "(250 + 50 delivery) + 18% VAT, worked out by the server, not taken from the caller");
    assert_eq!(
        (r1["status"].clone(), r1["gateway"].clone(), r1["payment_reference"].clone(), r1["requested_by"].clone()),
        (json!("Requested"), json!("fakepay"), json!("pi_b"), json!("manager@shop.test"))
    );
    assert_eq!(r1["lines"][0]["item_name"], json!("Blue Mug"));
    assert!(r1["number"].as_str().unwrap().starts_with("REFUND-"));
    let more = t.k.insert(&manager, "OrderRefund", obj(ask(json!([{"sales_order_line": mug_line, "qty": "2"}]), false))).await;
    assert!(
        matches!(&more, Err(Error::Validation(f)) if f.iter().any(|x| x.message.contains("at most 1 of Blue Mug"))),
        "the one asked for is spoken for: {more:?}"
    );
    let edit = t.k.update(&manager, "OrderRefund", r1id, obj(json!({"include_shipping": false})), None).await;
    assert!(edit.is_err(), "a refund asked for is not edited: decline it and ask again: {edit:?}");

    // Who decides.
    let clerk_ask = t.k.insert(&clerk, "OrderRefund", obj(ask(json!([{"sales_order_line": tee_line, "qty": "1"}]), false))).await;
    let clerk_ask = clerk_ask.unwrap_or_else(|e| panic!("a Sales User asks: {e:?}"));
    let by_clerk = t.k.run_action(&clerk, "OrderRefund", uid(&clerk_ask), "approve_refund", json!({})).await;
    assert!(matches!(by_clerk, Err(Error::Forbidden(_))), "asking is not approving: {by_clerk:?}");
    t.k.run_action(&manager, "OrderRefund", uid(&clerk_ask), "decline", json!({"note": "Not eligible"})).await.expect("declined");
    assert_eq!(t.get("OrderRefund", uid(&clerk_ask)).await["status"], json!("Declined"));
    let by_gateway = t.k.run_action(&fakepay, "OrderRefund", r1id, "approve_refund", json!({})).await;
    assert!(matches!(by_gateway, Err(Error::Forbidden(_))), "a gateway never approves a refund: {by_gateway:?}");
    let early = t.k.run_action(&fakepay, "OrderRefund", r1id, "refund_done", json!({"refund_reference": "re_1"})).await;
    assert!(matches!(early, Err(Error::Conflict(_))), "nor reports one nobody approved: {early:?}");
    t.k.run_action(&manager, "OrderRefund", r1id, "approve_refund", json!({})).await.expect("a manager approves");
    let approved = t.get("OrderRefund", r1id).await;
    assert_eq!((approved["status"].clone(), approved["approved_by"].clone()), (json!("Approved"), json!("manager@shop.test")));
    let wrong = t.k.run_action(&otherpay, "OrderRefund", r1id, "refund_done", json!({"refund_reference": "re_1"})).await;
    assert!(matches!(wrong, Err(Error::Forbidden(_))), "only the gateway the order was paid with reports it: {wrong:?}");
    assert_eq!(t.available("blue-mug").await, n("8"), "nothing is back on the shelf until the refund is done");

    // The gateway refuses once, then sends it.
    t.k.run_action(&fakepay, "OrderRefund", r1id, "refund_failed", json!({"error": "card closed"})).await.expect("refused at the provider");
    let failed = t.get("OrderRefund", r1id).await;
    assert_eq!((failed["status"].clone(), failed["refund_error"].clone()), (json!("Refund Failed"), json!("card closed")));
    t.k.run_action(&manager, "OrderRefund", r1id, "approve_refund", json!({})).await.expect("approved again");
    let no_reference = t.k.run_action(&fakepay, "OrderRefund", r1id, "refund_done", json!({})).await;
    assert!(matches!(no_reference, Err(Error::Validation(_))), "the gateway says what it sent: {no_reference:?}");
    t.k.run_action(&fakepay, "OrderRefund", r1id, "refund_done", json!({"refund_reference": "re_b1"})).await.expect("refunded");
    t.k.run_action(&fakepay, "OrderRefund", r1id, "refund_done", json!({"refund_reference": "re_b1"})).await.expect("told twice, refunded once");
    let done = t.get("OrderRefund", r1id).await;
    assert_eq!(
        (done["status"].clone(), done["refund_reference"].clone(), done["booked"].clone(), done["booking_error"].clone()),
        (json!("Refunded"), json!("re_b1"), json!(true), Value::Null),
        "{done:?}"
    );
    assert_eq!(
        t.count("JournalEntry", json!({"remark": format!("Refund {} of web order {}", done["number"].as_str().unwrap(), b.number)})).await,
        1,
        "booked once"
    );

    // The books followed: the goods are back, the invoice credited, the money posted out and matched.
    assert_eq!(t.available("blue-mug").await, n("9"), "the returned mug is on the shelf");
    let back = t.get("DeliveryNote", done["delivery_note"].as_str().unwrap().parse().unwrap()).await;
    assert_eq!((back["is_return"].clone(), back["docstatus"].clone(), back["return_against"].clone()), (json!(true), json!(1), json!(dn.to_string())));
    let credit = t.get("SalesInvoice", done["credit_note"].as_str().unwrap().parse().unwrap()).await;
    assert_eq!((credit["is_return"].clone(), credit["docstatus"].clone(), dec(&credit["rounded_total"])), (json!(true), json!(1), n("-354")));
    assert_eq!(credit["items"].as_array().unwrap().len(), 2, "the mug and the delivery charge, not the tee");
    assert_eq!(outstanding(&t, uid(&credit)).await, Decimal::ZERO, "what the credit note owed the shopper has been paid back");
    let entry = t.get("JournalEntry", done["journal_entry"].as_str().unwrap().parse().unwrap()).await;
    assert_eq!((entry["docstatus"].clone(), dec(&entry["total_debit"])), (json!(1), n("354")));
    assert_eq!(dec(&t.get("PaymentRequest", b.request.unwrap()).await["refunded_amount"]), n("354"));
    let so_after = t.get("SalesOrder", b.so).await;
    assert_eq!(
        (so_after["commerce_payment_status"].clone(), so_after["status"].clone()),
        (json!("Partly Refunded"), json!("Closed")),
        "nothing is left to ship, and it must not hold stock"
    );
    let told = events(&t, "commerce.order_refunded").await;
    assert_eq!((told[0]["amount"].clone(), told[0]["email"].clone(), told[0]["notify"].clone()), (json!("354"), json!(GUEST), json!(true)));
    let refund_mails = |all: Vec<(String, String)>| all.into_iter().filter(|(subject, _)| subject.contains("refunded for order")).count();
    assert_eq!(refund_mails(mails(&t, GUEST).await), 1, "the shopper is told the money is on its way back");
    let seen = t.ok(None, "POST", "/orders/track", &[], Some(json!({"number": b.number, "token": b.token}))).await;
    assert_eq!(
        (seen["order"]["refunds"][0]["status"].clone(), dec(&seen["order"]["refunds"][0]["amount"])),
        (json!("Refunded"), n("354")),
        "the shopper sees it"
    );
    let twice = t.k.run_action(&manager, "OrderRefund", r1id, "post_books", json!({})).await;
    assert!(matches!(twice, Err(Error::Invalid(_))), "nothing left to post: {twice:?}");

    // The rest comes back: the second mug and the tee. Together the refunds are the payment, no more.
    let rest =
        t.k.insert(
            &manager,
            "OrderRefund",
            obj(ask(json!([{"sales_order_line": mug_line, "qty": "1"}, {"sales_order_line": tee_line, "qty": "1", "restock": false}]), false)),
        )
        .await;
    let rest = rest.unwrap_or_else(|e| panic!("the rest is asked for: {e:?}"));
    assert_eq!(dec(&rest["amount"]), n("767"), "(250 + 400) + 18% VAT");
    let beyond = t.k.insert(&manager, "OrderRefund", obj(ask(json!([{"sales_order_line": mug_line, "qty": "1"}]), false))).await;
    assert!(matches!(&beyond, Err(Error::Validation(f)) if f.iter().any(|x| x.message.contains("all come back already"))), "{beyond:?}");
    let shipping_again = t.k.insert(&manager, "OrderRefund", obj(ask(json!([{"sales_order_line": tee_line, "qty": "1"}]), true))).await;
    assert!(shipping_again.is_err(), "the delivery charge went back with the first one, and the tee is spoken for: {shipping_again:?}");
    let rid = uid(&rest);
    t.k.run_action(&manager, "OrderRefund", rid, "approve_refund", json!({})).await.unwrap();
    t.k.run_action(&fakepay, "OrderRefund", rid, "refund_done", json!({"refund_reference": "re_b2"})).await.unwrap();
    let rest = t.get("OrderRefund", rid).await;
    assert_eq!((rest["booked"].clone(), rest["booking_error"].clone()), (json!(true), Value::Null), "{rest:?}");
    assert_eq!(t.available("blue-mug").await, n("10"), "both mugs are back; the tee was not restocked");
    assert_eq!(t.available("logo-tee").await, n("4"), "a tee that cannot be sold again stays out of stock");
    let invoice = t.get("SalesInvoice", credit["return_against"].as_str().unwrap().parse().unwrap()).await;
    assert!(
        invoice["items"].as_array().unwrap().iter().all(|l| dec(&l["returned_qty"]) == dec(&l["qty"])),
        "each credit note counted on the invoice line it gave back: {invoice:?}"
    );
    let sent = t.get("DeliveryNote", dn).await;
    let back_on = |item: &str| sent["items"].as_array().unwrap().iter().find(|l| l["item_name"] == json!(item)).map(|l| dec(&l["returned_qty"]));
    assert_eq!((back_on("Blue Mug"), back_on("Logo Tee")), (Some(n("2")), Some(n("0"))), "both mugs came back on the note that sent them, the tee did not");
    let so_end = t.get("SalesOrder", b.so).await;
    assert_eq!(so_end["commerce_payment_status"], json!("Refunded"), "1121 paid, 1121 sent back");
    assert_eq!(dec(&t.get("PaymentRequest", b.request.unwrap()).await["refunded_amount"]), n("1121"));

    // ───────────── an order cancelled before it shipped: the payment goes back whole ─────────────
    let c = place(&t, json!([{"item": "blue-mug", "qty": 1}]), "ravi@example.com", "fakepay", "refund-c-0001").await;
    let before_pay = t.k.insert(&manager, "OrderRefund", obj(json!({"sales_order": c.so.to_string(), "kind": "Cancellation", "reason": reason["id"]}))).await;
    assert!(before_pay.is_err(), "nothing was paid, so nothing goes back: {before_pay:?}");
    pay(&t, &fakepay, c.request.unwrap(), c.total, "pi_c").await;
    assert_eq!(t.available("blue-mug").await, n("9"), "the order holds a mug");
    let cancel =
        t.k.insert(
            &manager,
            "OrderRefund",
            obj(
                json!({"sales_order": c.so.to_string(), "kind": "Cancellation", "reason": reason["id"], "lines": [{"sales_order_line": mug_line, "qty": "1"}]}),
            ),
        )
        .await;
    let cancel = cancel.unwrap_or_else(|e| panic!("a cancellation is asked for: {e:?}"));
    assert_eq!((dec(&cancel["amount"]), cancel["lines"].as_array().unwrap().len()), (n("354"), 0), "the whole payment, whatever lines were sent");
    let cid_ = uid(&cancel);
    let second = t.k.insert(&manager, "OrderRefund", obj(json!({"sales_order": c.so.to_string(), "kind": "Cancellation", "reason": reason["id"]}))).await;
    assert!(second.is_err(), "the payment is spoken for: {second:?}");
    t.k.run_action(&manager, "OrderRefund", cid_, "approve_refund", json!({})).await.unwrap();
    t.k.run_action(&fakepay, "OrderRefund", cid_, "refund_done", json!({"refund_reference": "re_c"})).await.unwrap();
    let cancelled = t.get("OrderRefund", cid_).await;
    assert_eq!(
        (cancelled["status"].clone(), cancelled["booked"].clone(), cancelled["booking_error"].clone()),
        (json!("Refunded"), json!(true), Value::Null),
        "{cancelled:?}"
    );
    let so_c = t.get("SalesOrder", c.so).await;
    assert_eq!((so_c["docstatus"].clone(), so_c["status"].clone(), so_c["commerce_payment_status"].clone()), (json!(2), json!("Cancelled"), json!("Refunded")));
    assert_eq!(
        t.get("PaymentEntry", t.get("PaymentRequest", c.request.unwrap()).await["payment_entry"].as_str().unwrap().parse().unwrap()).await["docstatus"],
        json!(2),
        "the payment was reversed in the books"
    );
    assert_eq!(t.available("blue-mug").await, n("10"), "the cancelled order let its mug go");

    // ───────────── cash on delivery: paid at the door, returned, refunded by hand ─────────────
    let d_ = place(&t, json!([{"item": "logo-tee", "qty": 1}]), "meera@example.com", "Cash on Delivery", "refund-d-0001").await;
    assert_eq!(t.get("SalesOrder", d_.so).await["commerce_payment_method"], json!("Cash on Delivery"));
    t.k.run_action(&manager, "SalesOrder", d_.so, "web_ship", json!({"invoice": "Yes", "notify": "No"})).await.expect("shipped before it is paid");
    let d_line = t.get("SalesOrder", d_.so).await["items"][0]["id"].as_str().unwrap().to_string();
    let none_yet = plan(json!({"sales_order": d_.so.to_string()})).await.unwrap();
    assert_eq!(none_yet["payable"], json!(false), "no money came in: {none_yet}");
    let unpaid =
        t.k.insert(
            &manager,
            "OrderRefund",
            obj(json!({"sales_order": d_.so.to_string(), "kind": "Return", "reason": reason["id"], "lines": [{"sales_order_line": d_line, "qty": "1"}]})),
        )
        .await;
    assert!(unpaid.is_err(), "no money came in, so none goes back: {unpaid:?}");
    t.k.run_action(&manager, "PaymentRequest", d_.request.unwrap(), "mark_paid", json!({"amount": d_.total.to_string(), "currency": "INR"}))
        .await
        .expect("the courier handed the cash over");
    assert_eq!(t.get("SalesOrder", d_.so).await["commerce_payment_status"], json!("Paid"));
    let hand = t.k.insert(&manager, "OrderRefund", obj(json!({"sales_order": d_.so.to_string(), "kind": "Return", "reason": reason["id"], "lines": [{"sales_order_line": d_line, "qty": "1"}], "notify_customer": false}))).await;
    let hand = hand.unwrap_or_else(|e| panic!("a return of an order paid in cash: {e:?}"));
    assert_eq!(
        (hand["gateway"].clone(), dec(&hand["amount"])),
        (json!("Cash on Delivery"), d_.total - n("59")),
        "the tee and its VAT, not the delivery charge"
    );
    let hid = uid(&hand);
    let plugin = t.k.run_action(&fakepay, "OrderRefund", hid, "refund_done", json!({"refund_reference": "re_x"})).await;
    assert!(plugin.is_err(), "no plugin refunds cash");
    let no_note = t.k.run_action(&manager, "OrderRefund", hid, "mark_refunded", json!({})).await;
    assert!(matches!(no_note, Err(Error::Validation(_))), "say how it was refunded: {no_note:?}");
    t.k.run_action(&manager, "OrderRefund", hid, "mark_refunded", json!({"note": "Handed back in cash at the shop", "refund_reference": "receipt 17"}))
        .await
        .expect("refunded by hand");
    let hand = t.get("OrderRefund", hid).await;
    assert_eq!(
        (hand["status"].clone(), hand["booked"].clone(), hand["approved_by"].clone(), hand["decision_note"].clone()),
        (json!("Refunded"), json!(true), json!("manager@shop.test"), json!("Handed back in cash at the shop")),
        "{hand:?}"
    );
    assert_eq!(t.available("logo-tee").await, n("4"), "the tee is back (it went out with the order, and came back)");
    assert_eq!(events(&t, "commerce.order_refunded").await.last().unwrap()["notify"], json!(false), "the shopper was not to be told");
    assert_eq!(refund_mails(mails(&t, "meera@example.com").await), 0, "and is not mailed");
}

// ───────────────────────────── variants: sizes and colours ─────────────────────────────

#[test]
fn variants_end_to_end() {
    let _db = DB.lock().unwrap_or_else(|e| e.into_inner());
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().thread_stack_size(16 << 20).build().unwrap();
    rt.block_on(async { tokio::spawn(variants()).await.unwrap() });
}

/// A storefront page as the platform hands the request to the package: (status, headers, body).
async fn html(t: &T, path: &str, query: &[(&str, &str)]) -> (u16, Vec<(String, String)>, String) {
    let mut headers = Map::new();
    headers.insert("x-ferrum-site-api".into(), json!("/api/site/variants/commerce"));
    headers.insert("x-ferrum-site-origin".into(), json!("https://shop.example.com"));
    let req = SiteRequest {
        tenant: t.ctx.tenant,
        user: None,
        method: "GET".into(),
        path: format!("/_site{path}"),
        query: query.iter().map(|(k, v)| (k.to_string(), json!(v))).collect(),
        headers,
        body: vec![],
        client_ip: "203.0.113.7".into(),
    };
    let r = t.site.handle(&t.k, req).await.unwrap_or_else(|e| panic!("GET {path}: {e:?}"));
    (r.status, r.headers, String::from_utf8(r.body).unwrap())
}

async fn variants() {
    let Shop { t, tenant: _, cid, stores, products, nos, selling, mug, tee } = small_shop("variants").await;
    // Three variants of the tee, each an item of its own with its own price and stock.
    let make = |code: &'static str, name: &'static str, rate: &'static str| {
        let (t, products, nos, selling) = (&t, &products, &nos, &selling);
        async move {
            let it = uid(&t.insert("Item", json!({"item_code": code, "item_name": name, "item_group": products["id"], "stock_uom": nos["id"]})).await);
            t.insert("ItemPrice", json!({"item": it.to_string(), "price_list": selling["id"], "rate": rate})).await;
            it
        }
    };
    let red_m = make("TEE-RED-M", "Logo Tee Red M", "400").await;
    let red_l = make("TEE-RED-L", "Logo Tee Red L", "420").await;
    let blue_m = make("TEE-BLUE-M", "Logo Tee Blue M", "390").await;
    let spare = make("SPARE", "Spare", "100").await;
    let stock = t
        .insert(
            "StockEntry",
            json!({"company": cid.to_string(), "purpose": "Opening Stock", "items": [
                {"item": red_m.to_string(), "qty": "5", "basic_rate": "150", "target_warehouse": stores["id"]},
                {"item": blue_m.to_string(), "qty": "3", "basic_rate": "150", "target_warehouse": stores["id"]}]}),
        )
        .await;
    t.submit("StockEntry", uid(&stock)).await;
    let parent = uid(&t.find_one("WebsiteItem", json!({"item": tee.to_string()})).await);
    let opts = |pairs: &[(&str, &str)]| Value::Array(pairs.iter().map(|(o, v)| json!({"option": o, "value": v})).collect());
    let variant =
        |item: Uuid, pairs: &[(&str, &str)]| json!({"item": item.to_string(), "published": true, "variant_of": parent.to_string(), "options": opts(pairs)});

    // ───────────── the rules ─────────────
    let first = t.insert("WebsiteItem", variant(red_m, &[("Colour", "Red"), ("Size", "M")])).await;
    assert_eq!((first["title"].clone(), first["slug"].clone()), (json!("Logo Tee — Red / M"), json!("logo-tee-red-m")), "named from the item and its choices");
    t.insert("WebsiteItem", variant(red_l, &[("Colour", "Red"), ("Size", "L")])).await;
    let bad = |label: &'static str, r: ferrum_kernel::Result<Doc>, field: &'static str, says: &'static str| match r {
        Err(Error::Validation(f)) if f.iter().any(|x| x.field == field && x.message.contains(says)) => {}
        other => panic!("{label}: expected {field} to say '{says}', got {other:?}"),
    };
    bad("twin", t.k.insert(&t.ctx, "WebsiteItem", obj(variant(spare, &[("colour", "RED"), ("size", "m")]))).await, "options", "already");
    bad("different options", t.k.insert(&t.ctx, "WebsiteItem", obj(variant(spare, &[("Colour", "Blue")]))).await, "options", "same options");
    bad(
        "no options",
        t.k.insert(&t.ctx, "WebsiteItem", obj(json!({"item": spare.to_string(), "variant_of": parent.to_string()}))).await,
        "options",
        "what makes",
    );
    bad("twice", t.k.insert(&t.ctx, "WebsiteItem", obj(variant(spare, &[("Size", "S"), ("size", "M")]))).await, "options", "twice");
    let of_a_variant = json!({"item": spare.to_string(), "variant_of": sid(&first), "options": [{"option": "Colour", "value": "Red"}]});
    bad("variant of a variant", t.k.insert(&t.ctx, "WebsiteItem", obj(of_a_variant)).await, "variant_of", "one level deep");
    bad("self", t.k.update(&t.ctx, "WebsiteItem", parent, obj(json!({"variant_of": parent.to_string()})), None).await, "variant_of", "own parent");
    let mug_wi = uid(&t.find_one("WebsiteItem", json!({"item": mug.to_string()})).await);
    bad(
        "a group that has variants joins another",
        t.k.update(&t.ctx, "WebsiteItem", parent, obj(json!({"variant_of": mug_wi.to_string(), "options": [{"option": "X", "value": "Y"}]})), None).await,
        "variant_of",
        "variants of its own",
    );
    t.insert("WebsiteItem", variant(blue_m, &[("Colour", "Blue"), ("Size", "M")])).await;

    // ───────────── browsing and search ─────────────
    let cat = t.get_pub("/catalog").await;
    let mut slugs: Vec<&str> = cat["items"].as_array().unwrap().iter().map(|c| c["slug"].as_str().unwrap()).collect();
    slugs.sort();
    assert_eq!(slugs, vec!["blue-mug", "logo-tee"], "a tee once, its variants under it: {cat}");
    let card = cat["items"].as_array().unwrap().iter().find(|c| c["slug"] == json!("logo-tee")).unwrap();
    assert_eq!(
        (card["from"].clone(), dec(&card["price"]["price"]), card["in_stock"].clone()),
        (json!(true), n("390"), json!(true)),
        "from the cheapest variant, its own price ignored"
    );
    assert!(card["available_qty"].is_null(), "no quantity of its own");
    for (query, found) in [
        ("q=red", 1),
        ("q=blue tee", 1),
        ("q=tee blue", 1),
        ("q=TEE-BLUE", 1),
        ("q=zzz", 0),
        ("q=mug", 1),
        ("min_price=380&max_price=395", 1),
        ("in_stock=1", 2),
    ] {
        assert_eq!(t.get_pub(&format!("/catalog?{query}")).await["items"].as_array().unwrap().len(), found, "{query}");
    }
    assert_eq!(t.get_pub("/catalog?sort=price_desc").await["items"][0]["slug"], json!("logo-tee"), "sorted by the group's price");
    assert_eq!(t.get_pub("/catalog?sort=price_asc").await["items"][0]["slug"], json!("blue-mug"));

    // ───────────── the item and its variants ─────────────
    let page = t.get_pub("/items/logo-tee").await;
    assert_eq!((page["variants"].as_array().unwrap().len(), page["purchasable"].clone()), (3, json!(false)), "sold through its variants");
    assert_eq!(page["variants"][0]["slug"], json!("logo-tee-red-m"));
    assert_eq!(page["variants"][0]["options"], json!([{"option": "Colour", "value": "Red"}, {"option": "Size", "value": "M"}]));
    assert_eq!((page["variants"][1]["in_stock"].clone(), page["variants"][2]["in_stock"].clone()), (json!(false), json!(true)), "red L has none");
    let one = t.get_pub("/items/logo-tee-red-m").await;
    assert_eq!((one["variant_of"]["slug"].clone(), one["purchasable"].clone()), (json!("logo-tee"), json!(true)));
    assert_eq!(t.available("logo-tee-red-m").await, n("5"));

    // ───────────── the page, and its choices as plain links ─────────────
    let (status, _, body) = html(&t, "/p/logo-tee", &[]).await;
    assert_eq!(status, 200);
    assert!(body.contains(r#"<legend>Colour: <strong>Red</strong></legend>"#) && body.contains(r#"<legend>Size: <strong>M</strong></legend>"#), "{body}");
    assert!(body.contains(r#"class="pick on" href="/p/logo-tee?v=logo-tee-red-m" aria-current="true" aria-label="Red">Red</a>"#), "{body}");
    assert!(body.contains(r#"class="pick off" href="/p/logo-tee?v=logo-tee-red-l" aria-label="L, sold out">L</a>"#), "sold out is said, not hidden: {body}");
    assert!(body.contains(r#"href="/p/logo-tee?v=logo-tee-blue-m""#), "blue keeps the size that exists");
    assert!(body.contains(r#"data-item="logo-tee-red-m""#) && body.contains(r#"<p class="variant-name">Red / M</p>"#) && body.contains("<h1>Logo Tee</h1>"));
    assert!(body.contains("INR 400.00"), "the chosen variant's price: {body}");
    let (_, _, blue) = html(&t, "/p/logo-tee", &[("v", "logo-tee-blue-m")]).await;
    assert!(blue.contains(r#"data-item="logo-tee-blue-m""#) && blue.contains("INR 390.00") && blue.contains("Blue / M"), "{blue}");
    let (_, _, sold_out) = html(&t, "/p/logo-tee", &[("v", "logo-tee-red-l")]).await;
    assert!(sold_out.contains(r#"<p class="out">Out of stock</p>"#) && !sold_out.contains(r#"class="add""#), "nothing to buy: {sold_out}");
    let (_, _, nonsense) = html(&t, "/p/logo-tee", &[("v", "<script>")]).await;
    assert!(
        nonsense.contains(r#"data-item="logo-tee-red-m""#) && !nonsense.contains("<script>"),
        "an unknown choice falls back to the first that can be bought"
    );
    let (status, headers, _) = html(&t, "/p/logo-tee-red-l", &[]).await;
    assert_eq!(
        (status, headers.iter().find(|(k, _)| k == "location").map(|(_, v)| v.as_str())),
        (301, Some("/p/logo-tee?v=logo-tee-red-l")),
        "a variant's address is its item's"
    );
    let (_, _, home) = html(&t, "/", &[]).await;
    assert!(home.contains(r#"<span class="from">From</span>"#) && !home.contains("logo-tee-red"), "{home}");
    let (_, _, map) = html(&t, "/sitemap.xml", &[]).await;
    assert!(map.contains("/p/logo-tee<") && !map.contains("logo-tee-red"), "variants are not pages of their own: {map}");

    // ───────────── listings say what to do next, and sorting is in the address ─────────────
    let (_, _, none) = html(&t, "/search", &[("q", "zzz")]).await;
    assert!(
        none.contains("<h2>No results for “zzz”</h2>") && none.contains(r#"class="refine""#) && none.contains(r#"<input type="hidden" name="q" value="zzz">"#),
        "{none}"
    );
    let (_, _, sorted) = html(&t, "/", &[("sort", "price_desc")]).await;
    assert!(
        sorted.contains(r#"<option value="price_desc" selected>"#) && sorted.contains(r#"<meta name="robots" content="noindex">"#),
        "a sorted page is not indexed: {sorted}"
    );
    let (_, _, plain) = html(&t, "/", &[]).await;
    assert!(!plain.contains(r#"<meta name="robots" content="noindex">"#), "the plain home is");
    let (_, _, stocked) = html(&t, "/", &[("in_stock", "1")]).await;
    assert!(stocked.contains(r#"name="in_stock" value="1" checked>"#), "{stocked}");
    let (_, _, cart_page) = html(&t, "/cart", &[]).await;
    assert!(
        cart_page.contains(r#"id="live""#)
            && cart_page.contains(r#"aria-busy="true""#)
            && cart_page.contains("Your cart, checkout and order work once it is on"),
        "{cart_page}"
    );

    // ───────────── buying one ─────────────
    let (cart, _) = t.new_cart(None, json!([{"item": "logo-tee-red-m", "qty": 2}])).await;
    assert_eq!(
        field_error(&t.cart(&cart, "POST", "/cart/items", Some(json!({"item": "logo-tee"}))).await, "item"),
        "choose one of its variants, such as a size or a colour"
    );
    assert_eq!(field_error(&t.cart(&cart, "POST", "/cart/items", Some(json!({"item": "logo-tee-red-l"}))).await, "qty"), "this item is out of stock");
    let view = t.cart_ok(&cart, "GET", "/cart", None).await;
    assert_eq!(view["items"][0]["title"], json!("Logo Tee — Red / M"));
    t.cart_ok(&cart, "PUT", "/cart/details", Some(details(GUEST, "Asha Rao", "IN"))).await;
    let placed = t.checkout(None, &cart, "variant-order-1", json!({"payment_method": "Cash on Delivery", "accept_terms": true})).await.expect("checkout");
    assert_eq!(placed["order"]["items"][0]["title"], json!("Logo Tee — Red / M"));
    assert_eq!(t.available("logo-tee-red-m").await, n("3"), "the order holds its two");
    assert_eq!(t.available("logo-tee-blue-m").await, n("3"), "the others are untouched");
    // Whoever bought a variant bought the item: the review of the item is a verified purchase.
    let review = t.insert("ItemReview", json!({"website_item": parent.to_string(), "rating": 5, "reviewer_name": "Asha", "reviewer_email": GUEST})).await;
    assert_eq!(review["verified_purchase"], json!(true));
    let stranger =
        t.insert("ItemReview", json!({"website_item": parent.to_string(), "rating": 4, "reviewer_name": "Zed", "reviewer_email": "zed@example.com"})).await;
    assert_eq!(stranger["verified_purchase"], json!(false));
}

#[test]
fn subscriptions_end_to_end() {
    let _db = DB.lock().unwrap_or_else(|e| e.into_inner());
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().thread_stack_size(16 << 20).build().unwrap();
    rt.block_on(async { tokio::spawn(subscriptions()).await.unwrap() });
}

async fn subscriptions() {
    let Shop { t, tenant, cid, products, nos, selling, .. } = shop_with("subs", &["commerce", "finance", "commerce_subscriptions"]).await;
    let inr = t.find_one("Currency", json!({"code": "INR"})).await;
    // ───────────── a coffee club sold as a monthly plan ─────────────
    let club = uid(&t
        .insert("Item", json!({"item_code": "CLUB", "item_name": "Coffee Club", "item_group": products["id"], "stock_uom": nos["id"], "is_stock_item": false}))
        .await);
    t.insert("ItemPrice", json!({"item": club.to_string(), "price_list": selling["id"], "rate": "300"})).await;
    let plan = uid(&t
        .insert(
            "SubscriptionPlan",
            json!({"plan_name": "Coffee Monthly", "item": club.to_string(), "currency": inr["id"], "price": "300", "billing_interval": "Month"}),
        )
        .await);
    t.insert("WebsiteItem", json!({"item": club.to_string(), "published": true, "csub_plan": plan.to_string()})).await;
    let fakepay = {
        let role = "plugin:fakepay".to_string();
        for entity in ["PaymentRequest", "PaymentException", "OrderRefund"] {
            let p = PermissionDef { entity: entity.into(), read: true, write: true, ..Default::default() };
            t.k.set_role_permission(&t.ctx, &role, &p).await.unwrap();
        }
        let u = t.k.create_user(&t.ctx, "fakepay@plugins.ferrum.invalid", "Plugin: fakepay", None, &[role], false).await.unwrap();
        t.k.load_ctx(tenant, u, None, "plugin-fakepay".into()).await.unwrap()
    };
    let mira = {
        let u = t.k.create_user(&t.ctx, "mira@example.com", "Customer", Some("Correct-Horse-9"), &[], false).await.unwrap();
        t.k.load_ctx(tenant, u, None, "portal".into()).await.unwrap()
    };

    // ───────────── the web order pays the first month; Finance bills from the second ─────────────
    let (cart, _) = t.new_cart(Some(&mira), json!([{"item": "coffee-club", "qty": "1"}])).await;
    t.ok(Some(&mira), "PUT", "/cart/details", &[("x-cart-token", cart.as_str())], Some(details("mira@example.com", "Mira Shah", "IN"))).await;
    let done = t.checkout(Some(&mira), &cart, "sub-order-0001", json!({"payment_method": "fakepay", "accept_terms": true})).await.expect("checkout");
    let request: Uuid = done["payment_request"]["id"].as_str().unwrap().parse().unwrap();
    pay(&t, &fakepay, request, dec(&done["order"]["totals"]["rounded_total"]), "pi_order").await;
    let so = uid(&t.find_one("SalesOrder", json!({"number": done["order"]["number"]})).await);
    let sub = t.get("Subscription", uid(&t.find_one("Subscription", json!({"csub_sales_order": so.to_string()})).await)).await;
    let today = chrono::Utc::now().date_naive();
    let next_month = today.checked_add_months(chrono::Months::new(1)).unwrap();
    assert_eq!(sub["start_date"], json!(next_month.to_string()), "billing starts after the month the order paid for");
    assert_eq!((sub["plans"][0]["plan"].clone(), dec(&sub["plans"][0]["qty"])), (json!(plan.to_string()), n("1")));
    let customer: Uuid = sub["customer"].as_str().unwrap().parse().unwrap();

    // ───────────── a renewal: Finance's invoice is asked for through the order's method ─────────────
    // A month on, as far as the subscription knows: its billing starts today.
    let sub_id = uid(&sub);
    t.k.update(&t.ctx, "Subscription", sub_id, obj(json!({"start_date": today.to_string()})), None).await.expect("billing from today");
    t.k.run_action(&t.ctx, "Subscription", sub_id, "generate_invoices", json!({})).await.expect("renewal invoiced");
    let invoice = t.get("SalesInvoice", uid(&t.find_one("SalesInvoice", json!({"fin_subscription": sub_id.to_string()})).await)).await;
    let renewal = t.get("PaymentRequest", uid(&t.find_one("PaymentRequest", json!({"sales_invoice": sid(&invoice)})).await)).await;
    assert_eq!(
        (renewal["gateway"].clone(), dec(&renewal["amount"]), renewal["payer_email"].clone(), renewal["status"].clone()),
        (json!("fakepay"), dec(&invoice["outstanding_amount"]), json!("mira@example.com"), json!("Initiated"))
    );
    assert!(dec(&renewal["amount"]) > Decimal::ZERO);
    let mine = t.ok(Some(&mira), "GET", "/subscriptions", &[], None).await;
    let mine = &mine["subscriptions"][0];
    assert_eq!(
        (mine["number"].clone(), mine["plans"][0]["plan"].clone(), mine["entitled"].clone()),
        (sub["number"].clone(), json!("Coffee Monthly"), json!(true))
    );
    assert_eq!(mine["to_pay"][0]["invoice"], invoice["number"]);
    assert_eq!(mine["to_pay"][0]["payment_request"]["id"], json!(sid(&renewal)));
    // Asking to pay it the same way gives the same request.
    let again = t
        .ok(
            Some(&mira),
            "POST",
            &format!("/subscriptions/{}/pay", sub["number"].as_str().unwrap()),
            &[],
            Some(json!({"invoice": invoice["number"], "payment_method": "fakepay"})),
        )
        .await;
    assert_eq!(again["payment_request"]["id"], json!(sid(&renewal)));
    let total = dec(&renewal["amount"]);
    pay(&t, &fakepay, uid(&renewal), total, "pi_renewal").await;
    let paid = t.get("SalesInvoice", uid(&invoice)).await;
    assert_eq!(dec(&paid["outstanding_amount"]), Decimal::ZERO, "the renewal's payment is the invoice's");
    assert_eq!(events(&t, "commerce.invoice_paid").await.len(), 1);
    assert_eq!(t.ok(Some(&mira), "GET", "/subscriptions", &[], None).await["subscriptions"][0]["to_pay"], json!([]));
    {
        let mut tx = t.k.db.tenant_tx(tenant).await.unwrap();
        assert!(ferrum_app_commerce::entitled(&mut tx, tenant, customer, plan).await.unwrap(), "an active subscription gives the club");
    }

    // ───────────── part of a payment goes back, as the app that took it decides ─────────────
    let refund = {
        let mut tx = t.k.db.tenant_tx(tenant).await.unwrap();
        let r = ferrum_app_commerce::refund_payment(&t.k, &t.ctx, &mut tx, uid(&renewal), n("100"), None, "A goodwill gesture").await.expect("asked for");
        let too_much = ferrum_app_commerce::refund_payment(&t.k, &t.ctx, &mut tx, uid(&renewal), total, None, "Everything").await;
        assert!(too_much.is_err(), "no more than the payment holds: {too_much:?}");
        tx.commit().await.unwrap();
        uid(&r)
    };
    let asked = t.get("OrderRefund", refund).await;
    assert_eq!(
        (asked["kind"].clone(), asked["status"].clone(), dec(&asked["amount"]), asked["gateway"].clone()),
        (json!("Payment"), json!("Requested"), n("100"), json!("fakepay"))
    );
    let by_hand = t.k.insert(&t.ctx, "OrderRefund", obj(json!({"kind": "Payment", "sales_order": so.to_string()}))).await;
    assert!(by_hand.is_err(), "a payment's refund is asked for by the app: {by_hand:?}");
    t.k.run_action(&t.ctx, "OrderRefund", refund, "approve_refund", json!({})).await.expect("approved");
    t.k.run_action(&fakepay, "OrderRefund", refund, "refund_done", json!({"refund_reference": "re_1"})).await.expect("refunded");
    let done = t.get("OrderRefund", refund).await;
    assert_eq!((done["status"].clone(), done["booked"].clone()), (json!("Refunded"), json!(true)), "{}", done["booking_error"]);
    assert!(!done["journal_entry"].is_null(), "the money out is in the books");
    assert_eq!(dec(&t.get("PaymentRequest", uid(&renewal)).await["refunded_amount"]), n("100"));
    assert_eq!(events(&t, "commerce.payment_refunded").await.len(), 1);

    // ───────────── the customer cancels: it runs to the end of the month paid for ─────────────
    let cancelled = t.ok(Some(&mira), "POST", &format!("/subscriptions/{}/cancel", sub["number"].as_str().unwrap()), &[], None).await;
    assert_eq!(cancelled["cancellation_date"], json!((next_month - chrono::Duration::days(1)).to_string()));
    let others = t.call(None, "GET", "/subscriptions", &[], None).await;
    assert!(matches!(others, Err(Error::Unauthorized(_))), "{others:?}");

    // ───────────── an order cancelled before it is paid cancels what it started ─────────────
    let unpaid = place(&t, json!([{"item": "coffee-club", "qty": "2"}]), "omar@example.com", "Cash on Delivery", "sub-order-0002").await;
    let second = t.get("Subscription", uid(&t.find_one("Subscription", json!({"csub_sales_order": unpaid.so.to_string()})).await)).await;
    assert_eq!(dec(&second["plans"][0]["qty"]), n("2"));
    t.k.cancel(&t.ctx, "SalesOrder", unpaid.so, None).await.expect("order cancelled");
    assert!(!t.get("Subscription", uid(&second)).await["cancellation_date"].is_null(), "the subscription went with its order");
    let _ = cid;
}

fn sid(doc: &Doc) -> String {
    doc["id"].as_str().unwrap().to_string()
}

#[test]
fn order_edits_end_to_end() {
    let _db = DB.lock().unwrap_or_else(|e| e.into_inner());
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().thread_stack_size(16 << 20).build().unwrap();
    rt.block_on(async { tokio::spawn(order_edits()).await.unwrap() });
}

/// The desk changes an order before it is fulfilled: quantities, lines, the address. An unpaid order's
/// request is replaced; a paid one is asked for the difference, or sent it back; the shopper is told; what
/// has shipped cannot be taken off.
async fn order_edits() {
    let Shop { t, tenant, cid, tee, .. } = shop_with("order-edits", &["commerce", "crm"]).await;
    let output_tax = t.find_one("Account", json!({"account_name": "Output Tax", "company": cid.to_string()})).await;
    let reduced = t
        .insert(
            "TaxTemplate",
            json!({"title": "VAT 5%", "company": cid.to_string(), "kind": "Sales",
                   "rows": [{"charge_type": "On Net Total", "account": output_tax["id"], "description": "VAT", "rate": "5"}]}),
        )
        .await;
    ferrum_app_erp::localization::add(Arc::new(RegionTax(Mutex::new(Some(tenant)), uid(&reduced))));
    let fakepay = gateway(&t, "fakepay").await;
    let edit = |so: Uuid, params: Value| {
        let t = &t;
        async move { t.k.run_action(&t.ctx, "SalesOrder", so, "web_update_items", params).await }
    };

    // ───────────── unpaid: the request is replaced by one for the new total ─────────────
    let a = place(&t, json!([{"item": "blue-mug", "qty": 2}]), "edit@example.com", "fakepay", "edit-a-0001").await;
    assert_eq!(a.total, n("649"), "(500 + 50) at 18%");
    let done = edit(a.so, json!({"changes": r#"[{"line": 1, "qty": "1"}]"#})).await.expect("edited");
    assert_eq!((dec(&done["total"]), dec(&done["difference"])), (n("354"), n("-295")));
    assert_eq!(t.get("PaymentRequest", a.request.unwrap()).await["status"], json!("Cancelled"), "the request for the old total is closed");
    let request = uid(&t.find_one("PaymentRequest", json!({"sales_order": a.so.to_string(), "status": "Initiated"})).await);
    assert_eq!(dec(&t.get("PaymentRequest", request).await["amount"]), n("354"));
    let told = mails(&t, "edit@example.com").await;
    assert!(told.iter().any(|(s, h)| s.contains("was changed") && h.contains("354")), "{told:?}");
    assert_eq!(t.available("blue-mug").await, n("9"), "the mug taken off is free again");
    pay(&t, &t.ctx, request, n("354"), "fp_edit_1").await;

    // ───────────── paid, then more: asked for the difference through the same method ─────────────
    let more = edit(a.so, json!({"changes": format!(r#"[{{"item": "{tee}", "qty": 1}}]"#)})).await.expect("added");
    assert_eq!((dec(&more["total"]), dec(&more["difference"])), (n("826"), n("472")), "(250 + 400 + 50) at 18%");
    let again = uid(&t.find_one("PaymentRequest", json!({"sales_order": a.so.to_string(), "status": "Initiated"})).await);
    assert_eq!((dec(&t.get("PaymentRequest", again).await["amount"]), t.get("PaymentRequest", again).await["gateway"].clone()), (n("472"), json!("fakepay")));
    assert_eq!(dec(&t.get("SalesOrder", a.so).await["commerce_amount_owed"]), n("0"), "asked for, so not left owing");
    pay(&t, &t.ctx, again, n("472"), "fp_edit_2").await;
    assert_eq!(t.get("SalesOrder", a.so).await["commerce_payment_status"], json!("Paid"));

    // ───────────── paid, then less: sent back through the gateway, booked when it has gone ─────────────
    let less = edit(a.so, json!({"changes": r#"[{"line": 3, "qty": "0"}]"#})).await.expect("removed (the delivery charge is line 2)");
    assert_eq!(dec(&less["difference"]), n("-472"));
    let refund = t.find_one("OrderRefund", json!({"sales_order": a.so.to_string(), "kind": "Adjustment"})).await;
    assert_eq!((refund["status"].clone(), dec(&refund["amount"]), refund["gateway"].clone()), (json!("Approved"), n("472"), json!("fakepay")));
    t.k.run_action(&fakepay, "OrderRefund", uid(&refund), "refund_done", json!({"refund_reference": "re_edit"})).await.expect("sent back");
    let refund = t.get("OrderRefund", uid(&refund)).await;
    assert_eq!((refund["status"].clone(), refund["booked"].clone()), (json!("Refunded"), json!(true)), "{refund:?}");
    assert_eq!(dec(&t.get("SalesOrder", a.so).await["commerce_amount_owed"]), n("0"));
    assert_eq!(t.get("SalesOrder", a.so).await["commerce_payment_status"], json!("Partly Refunded"));

    // ───────────── part shipped: what has gone cannot come off; the rest can change ─────────────
    let b = place(&t, json!([{"item": "blue-mug", "qty": 1}, {"item": "logo-tee", "qty": 2}]), "part@example.com", "Cash on Delivery", "edit-b-0001").await;
    let so = t.get("SalesOrder", b.so).await;
    let tee_line = so["items"].as_array().unwrap().iter().find(|l| l["item_name"] == json!("Logo Tee")).unwrap()["id"].as_str().unwrap().to_string();
    t.k.run_action(&t.ctx, "SalesOrder", b.so, "web_ship", json!({"lines": format!(r#"[{{"line": "{tee_line}", "qty": "1"}}]"#), "notify": "No"}))
        .await
        .expect("shipped");
    let number_of = |name: &str| so["items"].as_array().unwrap().iter().position(|l| l["item_name"] == json!(name)).unwrap() + 1;
    let refused = edit(b.so, json!({"changes": format!(r#"[{{"line": {}, "qty": "0"}}]"#, number_of("Logo Tee"))})).await.unwrap_err().to_string();
    assert!(refused.contains("delivered") || refused.contains("shipped") || refused.contains("billed"), "refused in words: {refused}");
    edit(b.so, json!({"changes": format!(r#"[{{"line": {}, "qty": "0"}}]"#, number_of("Blue Mug"))})).await.expect("the mug that has not shipped can come off");
    assert_eq!(t.get("SalesOrder", b.so).await["commerce_fulfilment_status"], json!("Partly Fulfilled"));

    // ───────────── the address: tax follows where it now goes ─────────────
    let c = place(&t, json!([{"item": "blue-mug", "qty": 1}]), "move@example.com", "fakepay", "edit-c-0001").await;
    assert_eq!(c.total, n("354"));
    let moved = edit(c.so, json!({"address": {"line1": "5 Marine Drive", "city": "Mumbai", "state": "Maharashtra", "postal_code": "400001", "country": "IN"}}))
        .await
        .expect("moved");
    assert_eq!(dec(&moved["total"]), n("315"), "300 at 5% where the rule says");
    let so = t.get("SalesOrder", c.so).await;
    assert!(so["commerce_shipping_address"].as_str().unwrap().contains("Marine Drive"));
    let place_now = t.find_one("Address", json!({"id": so["ship_to"]})).await;
    assert_eq!(place_now["state"], json!("Maharashtra"));
    let refused = edit(c.so, json!({"address": {"line1": "1 Rue", "city": "Paris", "country": "FR"}})).await.unwrap_err().to_string();
    assert!(refused.contains("ship"), "{refused}");
}

#[test]
fn after_sales_end_to_end() {
    let _db = DB.lock().unwrap_or_else(|e| e.into_inner());
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().thread_stack_size(16 << 20).build().unwrap();
    rt.block_on(async { tokio::spawn(after_sales()).await.unwrap() });
}

/// The token of the link in an order's confirmation (the address is a secret value: it is read from the
/// sealed mail, and the mail waiting in the outbox holds a placeholder).
async fn manage_token(t: &T, email: &str, nth: usize) -> String {
    let mail = opened_mails(t, email).await.into_iter().filter(|(s, _)| s.starts_with("Your order")).nth(nth).expect("a confirmation");
    let html = mail.1.replace("&#x2f;", "/");
    let at = html.find("/links/").expect("a link in the confirmation") + "/links/".len();
    html[at..].split('"').next().unwrap().to_string()
}

/// Shoppers cancel or ask to return from the link in their confirmation; the desk decides; the refund
/// goes back to the payment or as store credit; each step tells the shopper.
async fn after_sales() {
    let slug = "after-sales";
    let Shop { t, cid, products, nos, selling, .. } = shop_with(slug, &["commerce"]).await;
    let fakepay = gateway(&t, "fakepay").await;
    let c = cid.to_string();
    let current = t.find_one("Account", json!({"account_name": "Current Liabilities", "company": c})).await;
    let liability = t.insert("Account", json!({"account_name": "Store Credit", "company": c, "parent_account": current["id"], "root_type": "Liability"})).await;
    let goodwill = t.find_one("Account", json!({"account_name": "Administrative Expenses", "company": c})).await;
    let kind = t
        .insert(
            "StoredValueKind",
            json!({"kind_name": "Store Credit", "company": c, "liability_account": liability["id"], "adjustment_account": goodwill["id"]}),
        )
        .await;
    let _ = (&products, &nos, &selling);
    let settings = uid(&t.find_one("StoreSettings", json!({})).await);
    let set_policy = |v: Value| {
        let t = &t;
        async move { t.k.update(&t.ctx, "StoreSettings", settings, obj(v), None).await.unwrap() }
    };
    set_policy(json!({"restocking_fee_percent": "10", "return_shipping_paid_by": "Store"})).await;
    let use_link = |token: &str, action: &str, params: Value| {
        let (t, token, action) = (&t, token.to_string(), action.to_string());
        async move { t.k.use_link(slug, &token, &action, params.as_object().unwrap(), None).await }
    };

    // ───────────── a shopper cancels before anything shipped ─────────────
    let a = place(&t, json!([{"item": "blue-mug", "qty": 2}]), "cancel@example.com", "fakepay", "after-a-0001").await;
    pay(&t, &t.ctx, a.request.unwrap(), a.total, "fp_after_a").await;
    let link_a = manage_token(&t, "cancel@example.com", 0).await;
    let waiting = mails(&t, "cancel@example.com").await;
    assert!(waiting.iter().all(|(_, h)| !h.contains(&link_a)) && waiting[0].1.contains("••••••"), "the token is not in the clear in the outbox: {waiting:?}");
    let page = t.k.link_page(slug, &link_a).await.unwrap().expect("a page");
    let offered = page.actions.iter().map(|a| a.1.clone()).collect::<Vec<_>>().join(", ");
    assert!(offered.contains("Cancel My Order") && offered.contains("Return Something"), "the link offers both: {offered}");
    // What was bought, line by line, and the store's terms in the workspace's own words with its settings filled in.
    assert_eq!(page.tables.len(), 1, "the lines are shown: {:?}", page.tables);
    let (what, columns, lines) = &page.tables[0];
    assert!(!what.is_empty() && columns.len() == 4 && lines.iter().any(|r| r[0] == "Blue Mug" && r[1] == "2"), "{:?}", page.tables);
    let terms = page.fields.iter().find(|(l, _)| l.contains("Terms")).map(|f| f.1.clone()).expect("the terms are on the page");
    assert!(
        terms.contains("within 24 hours")
            && terms.contains("within 14 days")
            && terms.contains("10%")
            && terms.contains("Store")
            && terms.contains("Original Payment"),
        "{terms}"
    );
    let ends: chrono::DateTime<chrono::Utc> = {
        let mut tx = t.k.db.tenant_tx(t.ctx.tenant).await.unwrap();
        let at = sqlx::query_scalar("select min(expires_at) from action_link where tenant_id = $1 and record_id = $2")
            .bind(t.ctx.tenant)
            .bind(a.so)
            .fetch_one(&mut *tx)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        at
    };
    assert!(
        ends > chrono::Utc::now() + chrono::Duration::days(40) && ends < chrono::Utc::now() + chrono::Duration::days(46),
        "the longest window: a return 14 days after a delivery that may be a month off: {ends}"
    );
    let out = use_link(&link_a, "web_shopper_cancel", json!({"reason": "bought twice"})).await.expect("cancelled").expect("an answer");
    assert_eq!(out["refunds"].as_array().unwrap().len(), 1, "{out}");
    let refund = t.find_one("OrderRefund", json!({"sales_order": a.so.to_string()})).await;
    assert_eq!(refund["status"], json!("Approved"), "approved with the cancellation, the gateway carries it out");
    t.k.run_action(&fakepay, "OrderRefund", uid(&refund), "refund_done", json!({"refund_reference": "re_after_a"})).await.expect("refunded");
    assert_eq!(t.get("SalesOrder", a.so).await["docstatus"], json!(2));
    let told: Vec<String> = mails(&t, "cancel@example.com").await.into_iter().map(|m| m.0).collect();
    assert!(told.iter().any(|s| s.contains("was cancelled")) && told.iter().any(|s| s.contains("refunded for order")), "{told:?}");
    let said = mails(&t, "cancel@example.com").await.into_iter().find(|m| m.0.contains("was cancelled")).unwrap().1;
    assert!(said.contains(&a.total.normalize().to_string()) && said.contains("Card"), "the mail says how much goes back and where: {said}");
    assert!(use_link(&link_a, "web_shopper_cancel", json!({})).await.is_err(), "a cancelled order cannot be cancelled again");

    // ───────────── the window: a cancellation too late is refused in words ─────────────
    let late = place(&t, json!([{"item": "blue-mug", "qty": 1}]), "late@example.com", "fakepay", "after-b-0001").await;
    t.sql(&format!("update e_sales_order set created_at = now() - interval '30 hours' where id = '{}'", late.so)).await;
    let link_late = manage_token(&t, "late@example.com", 0).await;
    let refused = use_link(&link_late, "web_shopper_cancel", json!({})).await.unwrap_err().to_string();
    assert!(refused.contains("24 hours") && refused.contains("passed"), "{refused}");

    // ───────────── a return: asked, approved, received, refunded to the payment less the fee ─────────────
    let b = place(&t, json!([{"item": "blue-mug", "qty": 2}, {"item": "logo-tee", "qty": 1}]), "return@example.com", "fakepay", "after-c-0001").await;
    pay(&t, &t.ctx, b.request.unwrap(), b.total, "fp_after_c").await;
    let link_b = manage_token(&t, "return@example.com", 0).await;
    let early = use_link(&link_b, "web_shopper_return", json!({})).await.unwrap_err().to_string();
    assert!(early.contains("delivered"), "nothing to send back before it is delivered: {early}");
    t.k.run_action(&t.ctx, "SalesOrder", b.so, "web_ship", json!({"notify": "No"})).await.expect("shipped and invoiced");
    let too_many = use_link(&link_b, "web_shopper_return", json!({"lines": "1x5"})).await.unwrap_err().to_string();
    assert!(too_many.contains("at most 2"), "{too_many}");
    let made = use_link(&link_b, "web_shopper_return", json!({"lines": "1x1", "reason": "wrong item sent", "note": "the handle is chipped"}))
        .await
        .expect("asked")
        .unwrap();
    let request = uid(&t.find_one("ReturnRequest", json!({"sales_order": b.so.to_string()})).await);
    assert_eq!(made["return_request"], json!(request.to_string()));
    let rr = t.get("ReturnRequest", request).await;
    let reason = t.get("ReturnReason", rr["reason"].as_str().unwrap().parse().unwrap()).await;
    assert_eq!(
        (rr["status"].clone(), reason["reason"].clone(), dec(&rr["restocking_fee_percent"]), rr["shipping_paid_by"].clone()),
        (json!("Requested"), json!("Wrong item sent"), n("10"), json!("Store"))
    );
    let again = use_link(&link_b, "web_shopper_return", json!({"lines": "1x2"})).await.unwrap_err().to_string();
    assert!(again.contains("at most 1"), "what is asked for already is not asked for twice: {again}");
    t.k.run_action(&t.ctx, "ReturnRequest", request, "approve_return", json!({"note": "Use the label in the box"})).await.expect("approved");
    t.k.run_action(&t.ctx, "ReturnRequest", request, "receive_return", json!({})).await.expect("received");
    let rr = t.get("ReturnRequest", request).await;
    let refund = t.get("OrderRefund", rr["refund"].as_str().unwrap().parse().unwrap()).await;
    assert_eq!(
        (refund["kind"].clone(), dec(&refund["amount"]), refund["destination"].clone()),
        (json!("Return"), n("265.5"), json!("Original Payment")),
        "250 less 10%, at 18%: {refund:?}"
    );
    t.k.run_action(&t.ctx, "OrderRefund", uid(&refund), "approve_refund", json!({})).await.expect("approved");
    t.k.run_action(&fakepay, "OrderRefund", uid(&refund), "refund_done", json!({"refund_reference": "re_after_c"})).await.expect("refunded");
    assert_eq!(t.get("ReturnRequest", request).await["status"], json!("Refunded"));
    let told: Vec<String> = mails(&t, "return@example.com").await.into_iter().map(|m| m.0).collect();
    for part in ["We have your return request", "is approved", "refunded for order"] {
        assert!(told.iter().any(|s| s.contains(part)), "{part} in {told:?}");
    }
    let declined = use_link(&link_b, "web_shopper_return", json!({"lines": "logo tee x1"})).await.expect("the tee asked for, by its name");
    let unknown = use_link(&link_b, "web_shopper_return", json!({"lines": "Teapot"})).await.unwrap_err().to_string();
    assert!(unknown.contains("no item called Teapot"), "{unknown}");
    let tee_request = uid(&t.find_one("ReturnRequest", json!({"sales_order": b.so.to_string(), "status": "Requested"})).await);
    let _ = declined;
    assert!(t.k.run_action(&t.ctx, "ReturnRequest", tee_request, "decline_return", json!({})).await.is_err(), "a decline says why");
    t.k.run_action(&t.ctx, "ReturnRequest", tee_request, "decline_return", json!({"note": "worn"})).await.expect("declined");
    assert!(mails(&t, "return@example.com").await.iter().any(|m| m.0.contains("was not approved") && m.1.contains("worn")));

    // ───────────── store credit: the refund is a new card, its code sent once ─────────────
    set_policy(json!({"refund_destination": "Store Credit", "store_credit_kind": sid(&kind), "restocking_fee_percent": "0"})).await;
    let d = place(&t, json!([{"item": "blue-mug", "qty": 1}]), "credit@example.com", "fakepay", "after-d-0001").await;
    pay(&t, &t.ctx, d.request.unwrap(), d.total, "fp_after_d").await;
    let link_d = manage_token(&t, "credit@example.com", 0).await;
    t.k.run_action(&t.ctx, "SalesOrder", d.so, "web_ship", json!({"notify": "No"})).await.expect("shipped");
    use_link(&link_d, "web_shopper_return", json!({})).await.expect("asked");
    let request = uid(&t.find_one("ReturnRequest", json!({"sales_order": d.so.to_string()})).await);
    assert_eq!(t.get("ReturnRequest", request).await["resolution"], json!("Store Credit"));
    t.k.run_action(&t.ctx, "ReturnRequest", request, "approve_return", json!({})).await.expect("approved");
    t.k.run_action(&t.ctx, "ReturnRequest", request, "receive_return", json!({})).await.expect("received");
    let refund = uid(&t.find_one("OrderRefund", json!({"sales_order": d.so.to_string()})).await);
    t.k.run_action(&t.ctx, "OrderRefund", refund, "approve_refund", json!({})).await.expect("approved: done at once, no gateway carries store credit");
    let refund = t.get("OrderRefund", refund).await;
    assert_eq!((refund["status"].clone(), refund["booked"].clone(), dec(&refund["amount"])), (json!("Refunded"), json!(true), n("295")), "{refund:?}");
    let credit = t.find_one("StoredValueAccount", json!({"kind": sid(&kind)})).await;
    assert_eq!(dec(&credit["balance"]), n("295"));
    let sent = opened_mails(&t, "credit@example.com").await.into_iter().find(|m| m.0.contains("store credit")).expect("the code was sent");
    assert!(sent.1.contains("<strong>"), "{sent:?}");
    let waiting = mails(&t, "credit@example.com").await.into_iter().find(|m| m.0.contains("store credit")).unwrap();
    assert!(waiting.1.contains("••••••"), "kept sealed while it waits: {waiting:?}");
    assert_eq!(t.get("ReturnRequest", request).await["status"], json!("Refunded"));

    // ───────────── the window of a return ─────────────
    t.sql(&format!("update e_delivery_note set created_at = now() - interval '20 days' where id in (select parent_id from e_delivery_note__items where sales_order = '{}')", d.so)).await;
    let gone = use_link(&link_d, "web_shopper_return", json!({})).await.unwrap_err().to_string();
    assert!(gone.contains("14 days") && gone.contains("passed") || gone.contains("sent back"), "{gone}");
}

#[test]
fn shipping_zones_end_to_end() {
    let _db = DB.lock().unwrap_or_else(|e| e.into_inner());
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().thread_stack_size(16 << 20).build().unwrap();
    rt.block_on(async { tokio::spawn(shipping_zones()).await.unwrap() });
}

/// Shipping rules are zones: countries, regions and postal ranges, with a free-over threshold; a recognised
/// customer is priced from their own price list.
async fn shipping_zones() {
    let Shop { t, tenant, mug, .. } = shop_with("shipping-zones", &["commerce"]).await;
    let metro = t
        .insert(
            "ShippingRule",
            json!({"label": "Metro courier", "basis": "Fixed", "amount": "30", "countries": [{"country_code": "IN"}], "regions": [{"region": "Karnataka"}],
                   "postal_ranges": [{"from_code": "560001", "to_code": "560099"}], "free_over": "1000"}),
        )
        .await;
    let options = |cart: String, state: &'static str, post: &'static str| {
        let t = &t;
        async move {
            let mut d = details("zone@example.com", "Zed Zone", "IN");
            d["shipping_address"]["state"] = json!(state);
            d["shipping_address"]["postal_code"] = json!(post);
            t.cart_ok(&cart, "PUT", "/cart/details", Some(d)).await;
            let o = t.cart_ok(&cart, "GET", "/cart/shipping-options", None).await;
            o["options"].as_array().unwrap().iter().map(|r| (r["label"].as_str().unwrap().to_string(), dec(&r["amount"]))).collect::<Vec<_>>()
        }
    };
    let (cart, _) = t.new_cart(None, json!([{"item": "blue-mug", "qty": 1}])).await;
    let in_zone = options(cart.clone(), "karnataka", "560 050").await;
    assert_eq!(in_zone, vec![("Standard".to_string(), n("50")), ("Metro courier".to_string(), n("30"))], "the zone's rate beside the country's");
    assert_eq!(options(cart.clone(), "Karnataka", "560100").await, vec![("Standard".to_string(), n("50"))], "outside the postal range");
    assert_eq!(options(cart.clone(), "Kerala", "560050").await, vec![("Standard".to_string(), n("50"))], "another region");
    let (big, _) = t.new_cart(None, json!([{"item": "blue-mug", "qty": 5}])).await;
    let free = options(big, "Karnataka", "560001").await;
    assert_eq!(free[1], ("Metro courier".to_string(), n("0")), "1250 of goods: free over 1000: {free:?}");
    assert_eq!(t.get("ShippingRule", uid(&metro)).await["free_over"], json!("1000"));
    let bad = t
        .k
        .insert(&t.ctx, "ShippingRule", obj(json!({"label": "Bad", "basis": "Fixed", "amount": "1", "regions": [{"country_code": "India", "region": "Goa"}]})))
        .await;
    assert!(bad.is_err(), "a country code is two letters");

    // ───────────── a customer's own price list ─────────────
    let inr = t.find_one("Currency", json!({"code": "INR"})).await;
    let wholesale = t.insert("PriceList", json!({"price_list_name": "Wholesale", "currency": inr["id"], "selling": true})).await;
    t.insert("ItemPrice", json!({"item": mug.to_string(), "price_list": wholesale["id"], "rate": "200"})).await;
    t.insert("Customer", json!({"customer_name": "Trade Buyer", "email": "trade@example.com", "default_price_list": wholesale["id"]})).await;
    let trade = {
        let u = t.k.create_user(&t.ctx, "trade@example.com", "Trade Buyer", Some("Correct-Horse-9"), &[], false).await.unwrap();
        t.k.load_ctx(tenant, u, None, "portal".into()).await.unwrap()
    };
    let (mine, view) = t.new_cart(Some(&trade), json!([{"item": "blue-mug", "qty": 1}])).await;
    assert_eq!(dec(&view["totals"]["subtotal"]), n("200"), "their own price list, not the store's 250");
    let (guest, view) = t.new_cart(None, json!([{"item": "blue-mug", "qty": 1}])).await;
    assert_eq!(dec(&view["totals"]["subtotal"]), n("250"));
    let _ = (mine, guest);
    let seen = t.ok(Some(&trade), "GET", "/items/blue-mug", &[], None).await;
    assert_eq!(dec(&seen["price"]["price"]), n("200"), "{seen}");
}

#[test]
fn shopper_mails_end_to_end() {
    let _db = DB.lock().unwrap_or_else(|e| e.into_inner());
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().thread_stack_size(16 << 20).build().unwrap();
    rt.block_on(async { tokio::spawn(shopper_mails()).await.unwrap() });
}

/// Each moment has its mail, a message template the workspace can edit, sent once.
async fn shopper_mails() {
    let Shop { t, cid, mug, stores, .. } = small_shop("shopper-mails").await;
    let fakepay = gateway(&t, "fakepay").await;
    let subjects = |to: &'static str| {
        let t = &t;
        async move { opened_mails(t, to).await.into_iter().map(|m| m.0).collect::<Vec<_>>() }
    };
    let has = |all: &[String], part: &str| all.iter().filter(|s| s.contains(part)).count();
    let settings = uid(&t.find_one("StoreSettings", json!({})).await);
    t.k.update(&t.ctx, "StoreSettings", settings, obj(json!({"out_for_delivery_words": "OUT_FOR_DELIVERY, with courier"})), None).await.unwrap();

    // ───────────── placed, failed (with a way to try again), paid ─────────────
    let a = place(&t, json!([{"item": "blue-mug", "qty": 1}]), "mails@example.com", "fakepay", "mails-a-0001").await;
    t.k.run_action(&fakepay, "PaymentRequest", a.request.unwrap(), "mark_failed", json!({"reason": "the card was declined"})).await.expect("failed");
    let all = subjects("mails@example.com").await;
    assert_eq!((has(&all, "Your order SO-"), has(&all, "did not go through")), (1, 1), "{all:?}");
    let failed = opened_mails(&t, "mails@example.com").await.into_iter().find(|m| m.0.contains("did not go through")).unwrap();
    assert!(failed.1.contains("declined") && failed.1.contains("shop.example.com"), "{failed:?}");
    pay(&t, &t.ctx, a.request.unwrap(), a.total, "fp_mails_a").await;
    pay(&t, &t.ctx, a.request.unwrap(), a.total, "fp_mails_a").await;
    assert_eq!(has(&subjects("mails@example.com").await, "Payment received"), 1, "once, not again for a repeated notice");

    // ───────────── shipped, out for delivery, delivered: when the carrier says ─────────────
    t.k.run_action(&t.ctx, "SalesOrder", a.so, "web_ship", json!({"carrier": "Fast Couriers", "tracking_number": "FC123"})).await.expect("shipped");
    assert_eq!(has(&subjects("mails@example.com").await, "has shipped"), 1);
    let dn = uid(&t.find_one("DeliveryNote", json!({"items.sales_order": a.so.to_string()})).await);
    let connection = t
        .insert(
            "CarrierConnection",
            json!({"connection_name": "Test", "company": cid.to_string(), "test": true,
                   "description": {"rate": {"url": "https://carrier.example/rates", "rates_at": "/rates", "service_at": "/service", "amount_at": "/amount", "currency_at": "/currency"},
                                   "label": {"url": "https://carrier.example/label", "tracking_at": "/tracking", "label_url_at": "/label"},
                                   "track": {"url": "https://carrier.example/track", "status_at": "/status", "statuses": {"DELIVERED": "Delivered"}}},
                   "fixtures": {"track": {"status": "DELIVERED"}}}),
        )
        .await;
    let shipment = t
        .insert(
            "CarrierShipment",
            json!({"company": cid.to_string(), "carrier": sid(&connection), "delivery_note": dn.to_string(), "packages": [{"weight": "1"}]}),
        )
        .await;
    let carrier_says = |status: &'static str, said: &'static str| {
        let (t, id) = (&t, uid(&shipment));
        async move {
            t.sql(&format!("update e_carrier_shipment set status = '{status}', tracking_status = '{said}', last_tracked_at = now(), delivery_note = '{dn}' where id = '{id}'")).await;
            ferrum_app_commerce::maintenance(&t.k).await.expect("upkeep");
        }
    };
    carrier_says("In Transit", "somewhere").await;
    assert_eq!(has(&subjects("mails@example.com").await, "out for delivery"), 0, "not out for delivery yet");
    carrier_says("In Transit", "out_for_delivery").await;
    carrier_says("In Transit", "OUT_FOR_DELIVERY").await;
    assert_eq!(has(&subjects("mails@example.com").await, "out for delivery"), 1, "the carrier's word, in any case, once");
    carrier_says("Delivered", "delivered").await;
    carrier_says("Delivered", "delivered").await;
    assert_eq!(has(&subjects("mails@example.com").await, "was delivered"), 1);

    // ───────────── back in stock: asked for, told once ─────────────
    let out = t.k.insert(&t.ctx, "StockEntry", obj(json!({"company": cid.to_string(), "purpose": "Material Issue", "items": [{"item": mug.to_string(), "qty": "8", "basic_rate": "90", "source_warehouse": stores["id"]}]}))).await.unwrap();
    t.submit("StockEntry", uid(&out)).await;
    let asked = t.call(None, "POST", "/items/blue-mug/stock-alert", &[], Some(json!({"email": "wait@example.com"}))).await;
    assert!(asked.is_err(), "it still has one in stock: {asked:?}");
    let all_out = t.k.insert(&t.ctx, "StockEntry", obj(json!({"company": cid.to_string(), "purpose": "Material Issue", "items": [{"item": mug.to_string(), "qty": "1", "basic_rate": "90", "source_warehouse": stores["id"]}]}))).await.unwrap();
    t.submit("StockEntry", uid(&all_out)).await;
    t.ok(None, "POST", "/items/blue-mug/stock-alert", &[], Some(json!({"email": "Wait@Example.com"}))).await;
    t.ok(None, "POST", "/items/blue-mug/stock-alert", &[], Some(json!({"email": "wait@example.com"}))).await;
    assert_eq!(t.count("StockAlert", json!({"email": "wait@example.com"})).await, 1, "asking twice is one alert");
    ferrum_app_commerce::maintenance(&t.k).await.expect("upkeep");
    assert!(subjects("wait@example.com").await.is_empty(), "still out of stock");
    let back = t.k.insert(&t.ctx, "StockEntry", obj(json!({"company": cid.to_string(), "purpose": "Material Receipt", "items": [{"item": mug.to_string(), "qty": "5", "basic_rate": "90", "target_warehouse": stores["id"]}]}))).await.unwrap();
    t.submit("StockEntry", uid(&back)).await;
    ferrum_app_commerce::maintenance(&t.k).await.expect("upkeep");
    ferrum_app_commerce::maintenance(&t.k).await.expect("upkeep");
    let told = opened_mails(&t, "wait@example.com").await;
    assert_eq!(told.len(), 1, "told once: {told:?}");
    assert!(told[0].0.contains("Blue Mug") && told[0].1.contains("&#x2f;p&#x2f;blue-mug"), "{told:?}");

    // ───────────── an abandoned cart: the event carries the consent; the reminder goes only with it ─────────────
    let remind = |email: &'static str, consent: bool| {
        let t = &t;
        async move {
            let (cart, _) = t.new_cart(None, json!([{"item": "blue-mug", "qty": 1}])).await;
            let mut d = details(email, "Cart Leaver", "IN");
            d["reminder_consent"] = json!(consent);
            t.cart_ok(&cart, "PUT", "/cart/details", Some(d)).await;
        }
    };
    remind("may@example.com", true).await;
    remind("mayhem@example.com", false).await;
    t.sql("update e_shopping_cart set last_activity = now() - interval '3 days' where status = 'Open'").await;
    ferrum_app_commerce::maintenance(&t.k).await.expect("upkeep");
    let seen = events(&t, "commerce.cart_abandoned").await;
    let consent_of = |email: &str| seen.iter().find(|e| e["email"] == json!(email)).map(|e| e["consent"].clone());
    assert_eq!((consent_of("may@example.com"), consent_of("mayhem@example.com")), (Some(json!(true)), Some(json!(false))));
    assert_eq!(subjects("may@example.com").await.iter().filter(|s| s.contains("left something in your cart")).count(), 1);
    assert!(subjects("mayhem@example.com").await.is_empty(), "no consent, no reminder");
}

#[test]
fn variants_generated_end_to_end() {
    let _db = DB.lock().unwrap_or_else(|e| e.into_inner());
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().thread_stack_size(16 << 20).build().unwrap();
    rt.block_on(async { tokio::spawn(variants_generated()).await.unwrap() });
}

/// A product with options is ERP's template item: its combinations are ERP's variants (items of their own),
/// each with a website item, a price and its own stock; the picker reads them; a value the store disables is
/// never sold.
async fn variants_generated() {
    let Shop { t, cid, tee, stores, .. } = shop_with("variants-generated", &["commerce", "stock_ops"]).await;
    let tee_site = t.find_one("WebsiteItem", json!({"item": tee.to_string()})).await;
    let generate = |params: Value| {
        let (t, id) = (&t, uid(&tee_site));
        async move { t.k.run_action(&t.ctx, "WebsiteItem", id, "generate_variants", params).await }
    };
    let none = generate(json!({})).await.unwrap_err().to_string();
    assert!(none.contains("what it comes in"), "{none}");
    let made = generate(json!({"options": {"Size": ["S", "M", "L"], "Colour": ["Red"]}, "disabled": r#"{"Size": ["L"]}"#})).await.expect("generated");
    assert_eq!((made["website_items_made"].clone(), made["unpublished"].clone()), (json!(3), json!(1)), "{made}");
    let variants = t.count("Item", json!({"stk_variant_of": tee.to_string()})).await;
    assert_eq!(variants, 3, "ERP made the combinations");
    let sites = t.count("WebsiteItem", json!({"variant_of": tee_site["id"]})).await;
    assert_eq!(sites, 3);

    // Generated again, nothing more is made; a new value adds its combination only.
    let again = generate(json!({"options": {"Size": ["S", "M", "L"], "Colour": ["Red"]}})).await.expect("again");
    assert_eq!(again["website_items_made"], json!(0), "{again}");
    let more = generate(json!({"options": {"Size": ["S", "XL"], "Colour": ["Red"]}})).await.expect("a new size");
    assert_eq!(more["website_items_made"], json!(1), "{more}");
    assert_eq!(t.count("Item", json!({"stk_variant_of": tee.to_string()})).await, 4);

    // Each has the template's price, and the picker offers what is sold.
    let medium = t.find_one("WebsiteItem", json!({"variant_of": tee_site["id"], "options.value": "M"})).await;
    let medium_item = medium["item"].as_str().unwrap().parse::<Uuid>().unwrap();
    let price = t.find_one("ItemPrice", json!({"item": medium_item.to_string()})).await;
    assert_eq!(dec(&price["rate"]), n("400"));
    let page = t.get_pub("/items/logo-tee").await;
    let shown: Vec<String> = page["variants"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|v| v["options"].as_array().unwrap().iter().filter_map(|o| o["value"].as_str().map(str::to_string)))
        .collect();
    let shown = shown.join(" ");
    assert!(shown.contains(" M") || shown.contains("M "), "{shown}");
    assert!(
        shown.split(' ').any(|v| v == "S") && shown.split(' ').any(|v| v == "M") && !shown.split(' ').any(|v| v == "L"),
        "the picker offers what is sold, not what the store disabled: {shown}"
    );

    // Its own stock: the medium is stocked, the small is not.
    let opening = t
        .insert(
            "StockEntry",
            json!({"company": cid.to_string(), "purpose": "Opening Stock", "items": [{"item": medium_item.to_string(), "qty": "4", "basic_rate": "150", "target_warehouse": stores["id"]}]}),
        )
        .await;
    t.submit("StockEntry", uid(&opening)).await;
    let small = t.find_one("WebsiteItem", json!({"variant_of": tee_site["id"], "options.value": "S"})).await;
    let in_m = t.get_pub(&format!("/items/{}", medium["slug"].as_str().unwrap())).await;
    let in_s = t.get_pub(&format!("/items/{}", small["slug"].as_str().unwrap())).await;
    assert_eq!((in_m["in_stock"].clone(), in_s["in_stock"].clone()), (json!(true), json!(false)), "stock per combination");

    // A disabled value cannot be published by hand.
    let large = t.find_one("WebsiteItem", json!({"variant_of": tee_site["id"], "options.value": "L"})).await;
    let refused = t.k.update(&t.ctx, "WebsiteItem", uid(&large), obj(json!({"published": true})), None).await.unwrap_err();
    assert!(format!("{refused:?}").contains("disabled"), "{refused:?}");
}

#[test]
fn updated_app_on_an_old_workspace_end_to_end() {
    let _db = DB.lock().unwrap_or_else(|e| e.into_inner());
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().thread_stack_size(16 << 20).build().unwrap();
    rt.block_on(async { tokio::spawn(old_workspace()).await.unwrap() });
}

/// A workspace that has not been updated has none of what an update seeds (the return reasons) and none of
/// the new settings (their columns are empty): everything still works, from the defaults in the code, and the
/// upkeep and its new steps find nothing missing.
async fn old_workspace() {
    let slug = "old-workspace";
    let Shop { t, .. } = small_shop(slug).await;
    t.sql("delete from e_return_reason").await;
    t.sql("update e_store_settings set cancel_window_hours = null, cancel_refund = null, return_window_days = null, return_shipping_paid_by = null, restocking_fee_percent = null, refund_destination = null, store_credit_kind = null, out_for_delivery_words = null, after_sales_text = null, store_languages = null, resend_per_day = null").await;
    let fakepay = gateway(&t, "fakepay").await;

    // The desk cancels a paid order: the reason it needs is made when it is not there.
    let a = place(&t, json!([{"item": "blue-mug", "qty": 1}]), "old-a@example.com", "fakepay", "old-a-0001").await;
    pay(&t, &t.ctx, a.request.unwrap(), a.total, "fp_old_a").await;
    let out = t.k.run_action(&t.ctx, "SalesOrder", a.so, "web_cancel", json!({})).await.expect("cancelled without the seeded reason");
    let refund = uid(&t.find_one("OrderRefund", json!({"sales_order": a.so.to_string()})).await);
    t.k.run_action(&fakepay, "OrderRefund", refund, "refund_done", json!({"refund_reference": "re_old"})).await.expect("done");
    assert_eq!(t.get("SalesOrder", a.so).await["docstatus"], json!(2), "{out}");

    // A shopper cancels and asks to return from the link, with the windows of the defaults.
    let b = place(&t, json!([{"item": "blue-mug", "qty": 2}]), "old-b@example.com", "Cash on Delivery", "old-b-0001").await;
    let link = manage_token(&t, "old-b@example.com", 0).await;
    t.k.run_action(&t.ctx, "SalesOrder", b.so, "web_ship", json!({"notify": "No"})).await.expect("shipped");
    let asked = t.k.use_link(slug, &link, "web_shopper_return", &obj(json!({"reason": "never heard of it"})), None).await.expect("asked");
    assert!(asked.is_some());
    let request = t.find_one("ReturnRequest", json!({"sales_order": b.so.to_string()})).await;
    assert_eq!(
        (request["resolution"].clone(), request["shipping_paid_by"].clone(), dec(&request["restocking_fee_percent"])),
        (json!("Original Payment"), json!("Shopper"), n("0"))
    );
    let reason = t.get("ReturnReason", request["reason"].as_str().unwrap().parse().unwrap()).await;
    assert_eq!(reason["reason"], json!("Other"), "no reason of that name: Other, made");

    // The edit's refund reason, and the upkeep's new steps.
    let c = place(&t, json!([{"item": "blue-mug", "qty": 2}]), "old-c@example.com", "fakepay", "old-c-0001").await;
    pay(&t, &t.ctx, c.request.unwrap(), c.total, "fp_old_c").await;
    t.k.run_action(&t.ctx, "SalesOrder", c.so, "web_update_items", json!({"changes": r#"[{"line": 1, "qty": "1"}]"#}))
        .await
        .expect("edited without the seeded reason");
    assert_eq!(t.count("OrderRefund", json!({"sales_order": c.so.to_string(), "kind": "Adjustment"})).await, 1);
    t.sql("update e_shopping_cart set last_activity = now() - interval '3 days' where status = 'Open'").await;
    ferrum_app_commerce::maintenance(&t.k).await.expect("upkeep");

    // The pages are English with no setting for languages, the shopper's link shows the order without terms to say, and
    // the desk's mail again falls back to three a day.
    let (lang, _, words) = words_of(&page_in(&t, "/cart", &[], Some("hi")).await.2);
    assert_eq!((lang.as_str(), words["cart.empty"].clone()), ("en", json!("Your cart is empty")));
    let page = t.k.link_page(slug, &manage_token(&t, "old-b@example.com", 0).await).await.unwrap().expect("a page");
    assert!(page.fields.iter().all(|(l, _)| !l.contains("Terms")) && page.tables.len() == 1, "{:?}", page.fields);
    t.k.run_action(&t.ctx, "SalesOrder", c.so, "web_ship", json!({"notify": "No"})).await.expect("shipped");
    t.k.run_action(&t.ctx, "SalesOrder", c.so, "web_resend_mail", json!({"mail": "Shipped"})).await.expect("sent again");
}

/// A shop whose gift card holds 500: sold through the store and its code taken from the mail, as a shopper
/// would have it. Returns the shop, the kind's id, the code and the account.
async fn card_shop(slug: &str) -> (Shop, Uuid, String, Uuid) {
    let shop = small_shop(slug).await;
    let Shop { t, cid, products, nos, selling, .. } = &shop;
    let c = cid.to_string();
    let current = t.find_one("Account", json!({"account_name": "Current Liabilities", "company": c})).await;
    let liability = t.insert("Account", json!({"account_name": "Gift Cards", "company": c, "parent_account": current["id"], "root_type": "Liability"})).await;
    let goodwill = t.find_one("Account", json!({"account_name": "Administrative Expenses", "company": c})).await;
    let kind = t
        .insert("StoredValueKind", json!({"kind_name": "Gift Card", "company": c, "liability_account": liability["id"], "adjustment_account": goodwill["id"]}))
        .await;
    let settings = t.find_one("StoreSettings", json!({})).await;
    t.k.update(&t.ctx, "StoreSettings", uid(&settings), obj(json!({"gift_card_kind": sid(&kind)})), None).await.unwrap();
    let card_item = t
        .insert(
            "Item",
            json!({"item_code": "GIFT500", "item_name": "Gift Card 500", "item_group": products["id"], "stock_uom": nos["id"], "is_stock_item": false,
                   "stored_value_kind": sid(&kind)}),
        )
        .await;
    t.insert("ItemPrice", json!({"item": sid(&card_item), "price_list": selling["id"], "rate": "500"})).await;
    t.insert("WebsiteItem", json!({"item": sid(&card_item), "published": true})).await;
    let kind = uid(&kind);
    let (code, account) = buy_card(t, "card-buy-0001").await;
    (shop, kind, code, account)
}

/// Another card of 500, bought and paid for through the store: its code from the mail and its account.
async fn buy_card(t: &T, key: &str) -> (String, Uuid) {
    let giver = format!("{key}@giver.example.com");
    let before = t.count("StoredValueAccount", json!({})).await;
    let bought = place(t, json!([{"item": "gift-card-500", "qty": 1}]), &giver, "fakepay", key).await;
    pay(t, &t.ctx, bought.request.unwrap(), bought.total, &format!("fp_{key}")).await;
    let sent: Vec<_> = opened_mails(t, &giver).await.into_iter().filter(|(s, _)| s.contains("gift card")).collect();
    let code = sent[0].1.split("<strong>").nth(1).and_then(|r| r.split("</strong>").next()).unwrap().to_string();
    assert_eq!(t.count("StoredValueAccount", json!({})).await, before + 1);
    let q = ListQuery { sort: Some("created_at desc".into()), limit: Some(1), ..Default::default() };
    let account = t.k.list(&t.ctx, "StoredValueAccount", &q).await.unwrap().rows[0].clone();
    assert_eq!(dec(&account["balance"]), n("500"));
    (code, uid(&account))
}

/// An order the way 0.8.0 left one a gift card paid part of: invoiced at checkout, the card redeemed
/// against the invoice, and the rest still to pay (`rest` is the request of the rest, made by the caller).
async fn paid_part_the_old_way(t: &T, kind: Uuid, code: &str, placed: &Placed) {
    // The request for the whole of it, which an earlier release never made.
    t.k.cancel(&t.ctx, "PaymentRequest", placed.request.unwrap(), None).await.expect("the whole request goes");
    let made = t.k.run_action(&t.ctx, "SalesOrder", placed.so, "make_sales_invoice", json!({})).await.expect("invoiced");
    let si: Uuid = made["id"].as_str().unwrap().parse().unwrap();
    t.submit("SalesInvoice", si).await;
    let mut tx = t.k.db.tenant_tx(t.ctx.tenant).await.unwrap();
    let out = ferrum_app_erp::stored_value::redeem(&t.k, &t.ctx, &mut tx, kind, code, None, si, None, None, &format!("old-way-{}", placed.so))
        .await
        .expect("redeemed");
    tx.commit().await.unwrap();
    let (amount, pe) = (out["amount"].as_str().unwrap().to_string(), out["payment"].as_str().unwrap().to_string());
    let so = t.get("SalesOrder", placed.so).await;
    let sys = ferrum_domain::util::sys(&t.ctx);
    let request =
        t.k.insert(
            &sys,
            "PaymentRequest",
            obj(json!({"company": so["company"], "sales_order": placed.so.to_string(), "order_number": placed.number, "gateway": "Gift Card",
                       "currency": so["currency"], "currency_code": "INR", "amount": amount, "status": "Initiated"})),
        )
        .await
        .expect("a request");
    t.submit("PaymentRequest", uid(&request)).await;
    t.sql(&format!("update e_payment_request set status = 'Paid', paid_at = now(), payment_entry = '{pe}' where id = '{}'", uid(&request))).await;
    t.sql(&format!("update e_sales_order set ext = ext || jsonb_build_object('commerce_paid_by_card', '{amount}') where id = '{}'", placed.so)).await;
    t.sql(&format!(
        "update e_payment_entry set ext = ext || jsonb_build_object('commerce_sales_order', '{}', 'commerce_payment_request', '{}') where id = '{pe}'",
        placed.so,
        uid(&request)
    ))
    .await;
}

#[test]
fn the_rest_after_a_card_end_to_end() {
    let _db = DB.lock().unwrap_or_else(|e| e.into_inner());
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().thread_stack_size(16 << 20).build().unwrap();
    rt.block_on(async { tokio::spawn(the_rest_after_a_card()).await.unwrap() });
}

/// A gift card paid part of an order and the rest's payment did not come: the shopper may try again for what
/// is left, and an order the rest of which never arrives cancels, the card getting its value back.
async fn the_rest_after_a_card() {
    let (Shop { t, .. }, kind, code, _) = card_shop("rest-after-card").await;
    let fakepay = gateway(&t, "fakepay").await;
    let card = |a: Uuid| {
        let t = &t;
        async move { t.get("StoredValueAccount", a).await }
    };
    let tt = &t;
    let again = |p: &Placed, email: &str, method: &str| {
        let body = json!({"number": p.number, "email": email, "token": p.token, "payment_method": method});
        async move { tt.call(None, "POST", "/orders/pay", &[], Some(body)).await }
    };

    // ───────────── as 0.8.0 left it: the card paid 500 at checkout, the rest's payment fails, the shopper tries again ─────────────
    let old = place(&t, json!([{"item": "logo-tee", "qty": 1}]), "old@example.com", "fakepay", "rest-old-0001").await;
    assert_eq!(old.total, n("531"));
    paid_part_the_old_way(&t, kind, &code, &old).await;
    let retry = again(&old, "old@example.com", "fakepay").await.expect("the shopper may pay the rest");
    assert_eq!((dec(&retry["payment_request"]["amount"]), retry["payment_request"]["status"].clone()), (n("31"), json!("Initiated")), "31 is left: {retry}");
    let request: Uuid = retry["payment_request"]["id"].as_str().unwrap().parse().unwrap();
    assert_eq!(t.get("SalesOrder", old.so).await["commerce_payment_status"], json!("Pending"));
    t.k.run_action(&fakepay, "PaymentRequest", request, "mark_failed", json!({"reason": "declined"})).await.expect("failed");
    assert_eq!(t.get("SalesOrder", old.so).await["commerce_payment_status"], json!("Failed"), "a card's part is not the order paid");
    let retry = again(&old, "old@example.com", "fakepay").await.expect("and again");
    assert_eq!(dec(&retry["payment_request"]["amount"]), n("31"));
    pay(&t, &t.ctx, retry["payment_request"]["id"].as_str().unwrap().parse().unwrap(), n("31"), "fp_old_rest").await;
    assert_eq!(t.get("SalesOrder", old.so).await["commerce_payment_status"], json!("Paid"));
    let done = again(&old, "old@example.com", "fakepay").await;
    assert!(matches!(done, Err(Error::Invalid(ref m)) if m.contains("already paid")), "nothing is left to pay: {done:?}");
    let invoice = t.find_one("SalesInvoice", json!({"items.sales_order": old.so.to_string(), "is_return": false})).await;
    assert_eq!(dec(&invoice["outstanding_amount"]), n("0"));

    // ───────────── the rest never arrives, as 0.8.0 left it: the upkeep cancels, the card has its value back ─────────────
    let (code, new) = buy_card(&t, "card-buy-0002").await;
    let account = new;
    let lost = place(&t, json!([{"item": "logo-tee", "qty": 1}]), "lost@example.com", "fakepay", "rest-lost-0001").await;
    paid_part_the_old_way(&t, kind, &code, &lost).await;
    assert_eq!(dec(&card(account).await["balance"]), n("0"));
    let rest = again(&lost, "lost@example.com", "fakepay").await.expect("asked for the rest");
    let rest_request: Uuid = rest["payment_request"]["id"].as_str().unwrap().parse().unwrap();
    t.sql(&format!("update e_payment_request set expires_at = now() - interval '1 minute' where id = '{rest_request}'")).await;
    ferrum_app_commerce::maintenance(&t.k).await.expect("upkeep");
    assert_eq!(t.get("PaymentRequest", rest_request).await["status"], json!("Expired"));
    let so = t.get("SalesOrder", lost.so).await;
    assert_eq!((so["docstatus"].clone(), so["commerce_payment_status"].clone()), (json!(2), json!("Refunded")), "cancelled by the upkeep");
    assert_eq!(dec(&card(account).await["balance"]), n("500"), "the card has what it paid back");
    assert_eq!(t.count("SalesInvoice", json!({"items.sales_order": lost.so.to_string(), "docstatus": 1})).await, 0);

    // ───────────── held, and the rest fails: the shopper tries again for what is left ─────────────
    let (cart, _) = t.new_cart(None, json!([{"item": "logo-tee", "qty": 1}])).await;
    t.cart_ok(&cart, "PUT", "/cart/details", Some(details("held2@example.com", "Held", "IN"))).await;
    let done = t
        .checkout(None, &cart, "rest-held-0002", json!({"payment_method": "fakepay", "accept_terms": true, "gift_card": {"code": code}}))
        .await
        .expect("checkout");
    let number = done["order"]["number"].as_str().unwrap().to_string();
    let token = done["order_token"].as_str().unwrap().to_string();
    let first: Uuid = done["payment_request"]["id"].as_str().unwrap().parse().unwrap();
    assert_eq!(dec(&card(account).await["held"]), n("500"));
    // What the card did, on the answer to checkout and on the order: what it holds, until when, what it has left; never its code.
    let card_says = &done["order"]["gift_card"];
    assert_eq!((dec(&card_says["held"]), dec(&card_says["paid"]), dec(&card_says["balance_left"])), (n("500"), n("0"), n("0")), "{card_says}");
    assert!(card_says["held_until"].is_string(), "{card_says}");
    assert!(!done.to_string().contains(&code), "the code is nowhere in the answer");
    t.k.run_action(&fakepay, "PaymentRequest", first, "mark_failed", json!({"reason": "declined"})).await.expect("failed");
    let body = json!({"number": number, "email": "held2@example.com", "token": token, "payment_method": "fakepay"});
    let retry = t.ok(None, "POST", "/orders/pay", &[], Some(body)).await;
    assert_eq!(dec(&retry["payment_request"]["amount"]), n("31"), "the card still holds its part: {retry}");
    pay(&t, &t.ctx, retry["payment_request"]["id"].as_str().unwrap().parse().unwrap(), n("31"), "fp_held_rest").await;
    assert_eq!(dec(&card(account).await["balance"]), n("0"), "paid: the invoice took the card's value");
    let order = t.ok(None, "POST", "/orders/lookup", &[], Some(json!({"number": number, "email": "held2@example.com", "token": token}))).await["order"].clone();
    let card_says = &order["gift_card"];
    assert_eq!(
        (dec(&card_says["paid"]), dec(&card_says["held"]), dec(&card_says["balance_left"]), card_says["held_until"].clone()),
        (n("500"), n("0"), n("0"), Value::Null),
        "{card_says}"
    );
    // Paid two ways: the form's cancellation asks for one payment's refund, so it points at the order's own, which asks for each.
    let reason = t.find_one("ReturnReason", json!({"reason": "Changed their mind"})).await;
    let so_held = uid(&t.find_one("SalesOrder", json!({"number": number})).await);
    let old_path = t.k.insert(&t.ctx, "OrderRefund", obj(json!({"sales_order": so_held.to_string(), "kind": "Cancellation", "reason": reason["id"]}))).await;
    assert!(matches!(&old_path, Err(Error::Validation(f)) if f.iter().any(|x| x.field == "kind" && x.message.contains("Cancel Web Order"))), "{old_path:?}");

    // ───────────── held, the rest expires, the store keeps the order: the card is free and the whole is asked ─────────────
    let (code, new) = buy_card(&t, "card-buy-0003").await;
    let account = new;
    let settings = t.find_one("StoreSettings", json!({})).await;
    t.k.update(&t.ctx, "StoreSettings", uid(&settings), obj(json!({"cancel_unpaid_orders": false})), None).await.unwrap();
    let (cart, _) = t.new_cart(None, json!([{"item": "logo-tee", "qty": 1}])).await;
    t.cart_ok(&cart, "PUT", "/cart/details", Some(details("kept@example.com", "Kept", "IN"))).await;
    let done = t
        .checkout(None, &cart, "rest-kept-0001", json!({"payment_method": "fakepay", "accept_terms": true, "gift_card": {"code": code}}))
        .await
        .expect("checkout");
    let request: Uuid = done["payment_request"]["id"].as_str().unwrap().parse().unwrap();
    t.sql(&format!("update e_payment_request set expires_at = now() - interval '1 minute' where id = '{request}'")).await;
    ferrum_app_commerce::maintenance(&t.k).await.expect("upkeep");
    assert_eq!(dec(&card(account).await["held"]), n("0"), "let go");
    let body = json!({"number": done["order"]["number"], "email": "kept@example.com", "token": done["order_token"], "payment_method": "fakepay"});
    let retry = t.ok(None, "POST", "/orders/pay", &[], Some(body)).await;
    assert_eq!(dec(&retry["payment_request"]["amount"]), n("531"), "the card holds nothing now: all of it is asked");
}

#[test]
fn change_preview_end_to_end() {
    let _db = DB.lock().unwrap_or_else(|e| e.into_inner());
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().thread_stack_size(16 << 20).build().unwrap();
    rt.block_on(async { tokio::spawn(change_preview()).await.unwrap() });
}

/// What the desk is shown before saving a change is what saving does: the lines as priced (delivery again for
/// the value, weight and place), the difference and where it goes. Nothing is written by asking.
async fn change_preview() {
    let Shop { t, cid, .. } = small_shop("change-preview").await;
    let fakepay = gateway(&t, "fakepay").await;
    // Delivery by order value in Karnataka (free from 500), a flat charge in Maharashtra.
    let rule = t.find_one("ShippingRule", json!({"label": "Standard"})).await;
    t.k.update(
        &t.ctx,
        "ShippingRule",
        uid(&rule),
        obj(json!({"basis": "Order Amount", "bands": [{"from_value": "0", "to_value": "500", "amount": "60"}, {"from_value": "500", "to_value": "0", "amount": "0"}],
                   "regions": [{"region": "Karnataka"}]})),
        None,
    )
    .await
    .expect("rule");
    t.insert(
        "ShippingRule",
        json!({"label": "Western", "basis": "Fixed", "amount": "120", "countries": [{"country_code": "IN"}], "regions": [{"region": "Maharashtra"}]}),
    )
    .await;
    let _ = cid;
    let preview = |so: Uuid, mut body: Value| {
        let t = &t;
        async move {
            body["sales_order"] = json!(so.to_string());
            t.call(Some(&t.ctx), "POST", "/desk/change-preview", &[], Some(body)).await
        }
    };
    let save = |so: Uuid, body: Value| {
        let t = &t;
        async move { t.k.run_action(&t.ctx, "SalesOrder", so, "web_update_items", body).await.map_err(|e| format!("{e:?}")) }
    };
    // The same answer either way, but for the ids of what saving makes.
    let same = |seen: &Value, done: &Value| {
        let (mut a, mut b) = (seen.clone(), done.clone());
        for v in [&mut a, &mut b] {
            for k in ["payment_request", "refund", "preview"] {
                v.as_object_mut().unwrap().remove(k);
            }
            // A line the change adds is given its id when it is saved.
            for l in v["lines"].as_array_mut().unwrap() {
                l.as_object_mut().unwrap().remove("id");
            }
        }
        assert_eq!(a, b, "the preview and the save say the same");
    };
    let untouched = |so: Uuid, grand: Decimal, requests: usize| {
        let t = &t;
        async move {
            assert_eq!(dec(&t.get("SalesOrder", so).await["rounded_total"]), grand, "asking changed nothing on the order");
            assert_eq!(t.count("PaymentRequest", json!({"sales_order": so.to_string()})).await, requests, "and made no request");
            assert_eq!(t.count("OrderRefund", json!({"sales_order": so.to_string()})).await, 0, "or refund");
        }
    };

    // ───────────── unpaid: two mugs ship free; one does not, and the request for the new total replaces the old ─────────────
    let a = place(&t, json!([{"item": "blue-mug", "qty": 2}]), "prev-a@example.com", "fakepay", "prev-a-0001").await;
    assert_eq!(a.total, n("590"), "500 at 18%, delivery free");
    let mails_before = mails(&t, "prev-a@example.com").await.len();
    let cut = json!({"changes": r#"[{"line": 1, "qty": "1"}]"#});
    let seen = preview(a.so, cut.clone()).await.expect("previewed");
    untouched(a.so, a.total, 1).await;
    assert_eq!(mails(&t, "prev-a@example.com").await.len(), mails_before, "the shopper was told nothing");
    assert_eq!(
        (dec(&seen["total"]), dec(&seen["difference"]), seen["preview"].clone()),
        (n("365.8"), n("-224.2"), json!(true)),
        "(250 + 60 of delivery) at 18%: {seen}"
    );
    assert_eq!(seen["settlement"]["kind"], json!("charge"));
    assert_eq!(
        (dec(&seen["settlement"]["amount"]), seen["settlement"]["gateway"].clone(), seen["settlement"]["replaces_open_request"].clone()),
        (n("365.8"), json!("fakepay"), json!(true))
    );
    let delivery: Vec<_> = seen["lines"].as_array().unwrap().iter().filter(|l| l["delivery"] == json!(true)).collect();
    assert_eq!((delivery.len(), dec(&delivery[0]["amount"])), (1, n("60")), "delivery is priced again, a line that was not there");
    let done = save(a.so, cut).await.expect("saved");
    same(&seen, &done);
    let so = t.get("SalesOrder", a.so).await;
    assert_eq!(dec(&so["rounded_total"]), n("365.8"));
    assert_eq!(so["items"].as_array().unwrap().len(), 2);
    let request = uid(&t.find_one("PaymentRequest", json!({"sales_order": a.so.to_string(), "status": "Initiated"})).await);
    assert_eq!(dec(&t.get("PaymentRequest", request).await["amount"]), n("365.8"));
    pay(&t, &t.ctx, request, n("365.8"), "fp_prev_a").await;

    // ───────────── paid, then to Maharashtra: another method, 120, the difference asked for through the card ─────────────
    let west = json!({"address": {"line1": "5 Marine Drive", "city": "Mumbai", "state": "Maharashtra", "postal_code": "400001", "country": "IN"}});
    let seen = preview(a.so, west.clone()).await.expect("previewed");
    untouched(a.so, n("365.8"), 2).await;
    assert_eq!((dec(&seen["total"]), dec(&seen["difference"])), (n("436.6"), n("70.8")), "(250 + 120) at 18%: {seen}");
    assert_eq!(
        (seen["settlement"]["kind"].clone(), seen["settlement"]["replaces_open_request"].clone(), dec(&seen["settlement"]["amount"])),
        (json!("charge"), json!(false), n("70.8"))
    );
    let done = save(a.so, west).await.expect("saved");
    same(&seen, &done);
    let so = t.get("SalesOrder", a.so).await;
    assert_eq!(so["commerce_shipping_rule"], json!(sid(&t.find_one("ShippingRule", json!({"label": "Western"})).await)));

    // ───────────── paid, then less: sent back to where it was paid ─────────────
    let b = place(&t, json!([{"item": "blue-mug", "qty": 1}, {"item": "logo-tee", "qty": 1}]), "prev-b@example.com", "fakepay", "prev-b-0001").await;
    assert_eq!(b.total, n("767"), "(250 + 400) at 18%, delivery free from 500: 650 + 0");
    pay(&t, &t.ctx, b.request.unwrap(), b.total, "fp_prev_b").await;
    let take = json!({"changes": r#"[{"line": 2, "qty": "0"}]"#});
    let seen = preview(b.so, take.clone()).await.expect("previewed");
    untouched(b.so, b.total, 1).await;
    assert_eq!(seen["settlement"]["kind"], json!("refund"));
    assert_eq!((seen["settlement"]["gateway"].clone(), dec(&seen["settlement"]["amount"])), (json!("fakepay"), dec(&seen["difference"]).abs()));
    let done = save(b.so, take).await.expect("saved");
    same(&seen, &done);
    let refund = t.find_one("OrderRefund", json!({"sales_order": b.so.to_string(), "kind": "Adjustment"})).await;
    assert_eq!((dec(&refund["amount"]), refund["gateway"].clone()), (dec(&seen["settlement"]["amount"]), json!("fakepay")));
    t.k.run_action(&fakepay, "OrderRefund", uid(&refund), "refund_done", json!({"refund_reference": "re_prev"})).await.expect("sent back");

    // ───────────── what the save refuses, the preview refuses; and only the desk asks ─────────────
    let refused = preview(a.so, json!({"changes": r#"[{"line": 1, "qty": "-1"}]"#})).await;
    assert!(matches!(refused, Err(Error::Validation(_))), "a change the save refuses, the preview refuses: {refused:?}");
    untouched(a.so, dec(&t.get("SalesOrder", a.so).await["rounded_total"]), 3).await;
    let far = preview(a.so, json!({"address": {"line1": "1 Rue", "city": "Goa", "state": "Goa", "postal_code": "403001", "country": "IN"}})).await;
    assert!(matches!(far, Err(Error::Validation(ref f)) if f[0].field == "address"), "no method of the store reaches Goa: {far:?}");
    // Only the desk asks.
    let shopper =
        t.k.load_ctx(t.ctx.tenant, t.k.create_user(&t.ctx, "nobody@shop.test", "Nobody", None, &[], false).await.unwrap(), None, "nobody".into())
            .await
            .unwrap();
    assert!(t.call(Some(&shopper), "POST", "/desk/change-preview", &[], Some(json!({"sales_order": a.so.to_string(), "changes": "[]"}))).await.is_err());
    assert!(t.call(None, "POST", "/desk/change-preview", &[], Some(json!({"sales_order": a.so.to_string()}))).await.is_err());
}

#[test]
fn desk_pieces_end_to_end() {
    let _db = DB.lock().unwrap_or_else(|e| e.into_inner());
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().thread_stack_size(16 << 20).build().unwrap();
    rt.block_on(async { tokio::spawn(desk_pieces()).await.unwrap() });
}

/// The desk sends a shopper mail again (counted, limited, never one with a secret), and a store with no returns
/// has a link that lives as long as its cancellation window.
async fn desk_pieces() {
    let slug = "desk-pieces";
    let Shop { t, .. } = shop_with(slug, &["commerce"]).await;
    let settings = uid(&t.find_one("StoreSettings", json!({})).await);
    let a = place(&t, json!([{"item": "blue-mug", "qty": 1}]), "resend@example.com", "fakepay", "resend-a-0001").await;
    let again = |so: Uuid, mail: &'static str| {
        let t = &t;
        async move { t.k.run_action(&t.ctx, "SalesOrder", so, "web_resend_mail", json!({"mail": mail})).await }
    };
    assert!(again(a.so, "Shipped").await.is_err(), "nothing has shipped");
    assert!(again(a.so, "Payment Received").await.is_err(), "nothing was paid");
    pay(&t, &t.ctx, a.request.unwrap(), a.total, "fp_resend_a").await;
    t.k.run_action(&t.ctx, "SalesOrder", a.so, "web_ship", json!({"carrier": "Fast Couriers", "tracking_number": "FC123", "notify": "Yes"}))
        .await
        .expect("shipped");
    let count = |subject: &'static str| {
        let t = &t;
        async move { mails(t, "resend@example.com").await.iter().filter(|m| m.0.contains(subject)).count() }
    };
    assert_eq!((count("has shipped").await, count("Payment received").await), (1, 1));
    let out = again(a.so, "Shipped").await.expect("sent again");
    assert_eq!((out["sent"].clone(), out["resent"].clone(), out["left_today"].clone()), (json!(true), json!("1"), json!(1)), "{out}");
    let all = mails(&t, "resend@example.com").await;
    let shipped: Vec<_> = all.iter().filter(|m| m.0.contains("has shipped")).collect();
    assert!(shipped.len() == 2 && shipped[1].1.contains("FC123") && shipped[1].1.contains("Blue Mug"), "{shipped:?}");
    again(a.so, "Payment Received").await.expect("the payment, again");
    assert_eq!(count("Payment received").await, 2);
    again(a.so, "Shipped").await.expect("a third time is the limit, the first among them");
    let refused = again(a.so, "Shipped").await.unwrap_err().to_string();
    assert!(refused.contains("limit is 3"), "{refused}");
    assert_eq!(dec(&t.get("SalesOrder", a.so).await["commerce_mails_resent"]), n("3"), "counted on the order");
    for secret in ["Gift Card Issued", "Order Placed", "Store Credit Issued", "Cancelled"] {
        let e = again(a.so, secret).await.unwrap_err().to_string();
        assert!(e.contains("secret value"), "{secret}: {e}");
    }
    t.k.update(&t.ctx, "StoreSettings", settings, obj(json!({"resend_per_day": 4})), None).await.unwrap();
    again(a.so, "Shipped").await.expect("the limit is the store's");

    // ───────────── no returns: the link lives as long as the cancellation window ─────────────
    t.k.update(&t.ctx, "StoreSettings", settings, obj(json!({"return_window_days": 0})), None).await.unwrap();
    let b = place(&t, json!([{"item": "blue-mug", "qty": 1}]), "window@example.com", "fakepay", "resend-b-0001").await;
    let ends: chrono::DateTime<chrono::Utc> = {
        let mut tx = t.k.db.tenant_tx(t.ctx.tenant).await.unwrap();
        let at = sqlx::query_scalar("select expires_at from action_link where tenant_id = $1 and record_id = $2")
            .bind(t.ctx.tenant)
            .bind(b.so)
            .fetch_one(&mut *tx)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        at
    };
    assert!(ends < chrono::Utc::now() + chrono::Duration::days(3), "{ends}");
    let link = manage_token(&t, "window@example.com", 0).await;
    let page = t.k.link_page(slug, &link).await.unwrap().expect("a page");
    assert_eq!(page.actions.iter().map(|a| a.0.clone()).collect::<Vec<_>>(), ["web_shopper_cancel"], "only what the store allows is offered");
}

/// A storefront page as a visitor's browser asks for it: with what the browser says it speaks.
async fn page_in(t: &T, path: &str, query: &[(&str, &str)], accept: Option<&str>) -> (u16, Vec<(String, String)>, String) {
    let mut headers = Map::new();
    headers.insert("x-ferrum-site-api".into(), json!("/api/site/words/commerce"));
    headers.insert("x-ferrum-site-origin".into(), json!("https://shop.example.com"));
    if let Some(a) = accept {
        headers.insert("accept-language".into(), json!(a));
    }
    let req = SiteRequest {
        tenant: t.ctx.tenant,
        user: None,
        method: "GET".into(),
        path: format!("/_site{path}"),
        query: query.iter().map(|(k, v)| (k.to_string(), json!(v))).collect(),
        headers,
        body: vec![],
        client_ip: "203.0.113.7".into(),
    };
    let r = t.site.handle(&t.k, req).await.unwrap_or_else(|e| panic!("GET {path}: {e:?}"));
    (r.status, r.headers, String::from_utf8(r.body).unwrap())
}

/// The words a page carries, as the script reads them, and the language it says it is in.
fn words_of(html: &str) -> (String, String, Map<String, Value>) {
    let lang = html.split("<html lang=\"").nth(1).and_then(|r| r.split('"').next()).unwrap().to_string();
    let dir = html.split("dir=\"").nth(1).and_then(|r| r.split('"').next()).unwrap().to_string();
    let json = html
        .split("<script type=\"application/json\" id=\"store-words\">")
        .nth(1)
        .and_then(|r| r.split("</script>").next())
        .expect("the words are in the page");
    (lang, dir, serde_json::from_str::<Value>(json).unwrap().as_object().cloned().unwrap())
}

#[test]
fn storefront_words_end_to_end() {
    let _db = DB.lock().unwrap_or_else(|e| e.into_inner());
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().thread_stack_size(16 << 20).build().unwrap();
    rt.block_on(async { tokio::spawn(storefront_words()).await.unwrap() });
}

/// Every page carries the words of one language, chosen by the store's languages and the visitor's preference,
/// from the app's English and a workspace's own words, English under every word a language lacks.
async fn storefront_words() {
    let Shop { t, .. } = small_shop("words").await;
    let settings = uid(&t.find_one("StoreSettings", json!({})).await);

    // ───────────── English, as it ships ─────────────
    let (status, headers, html) = page_in(&t, "/cart", &[], Some("hi-IN,hi;q=0.9")).await;
    let (lang, dir, en) = words_of(&html);
    assert_eq!((status, lang.as_str(), dir.as_str()), (200, "en", "ltr"), "the store offers English only");
    assert_eq!(en["cart.empty"], json!("Your cart is empty"));
    assert!(headers.iter().any(|(k, v)| k == "vary" && v == "accept-language"), "{headers:?}");

    // ───────────── a language the workspace writes ─────────────
    t.k.update(&t.ctx, "StoreSettings", settings, obj(json!({"store_languages": "en, hi, ar"})), None).await.unwrap();
    t.insert(
        "StorefrontWords",
        json!({"language": "HI", "words": {"cart.empty": "आपकी टोकरी खाली है", "cart.count_aria": "{n} आपकी टोकरी में", "cart.start": "</script><i>x</i>"}}),
    )
    .await;
    t.insert("StorefrontWords", json!({"language": "ar", "words": {"cart.empty": "سلتك فارغة"}})).await;
    let hindi = page_in(&t, "/cart", &[], Some("hi-IN,hi;q=0.9,en;q=0.5")).await.2;
    assert!(!hindi.contains("</script><i>"), "a word cannot end the script it is in");
    let (lang, dir, hi) = words_of(&hindi);
    assert_eq!(hi["cart.start"], json!("</script><i>x</i>"), "and reaches the script as written");
    assert_eq!((lang.as_str(), dir.as_str()), ("hi", "ltr"), "hi-IN is hi for a store that offers hi");
    assert_eq!(hi["cart.empty"], json!("आपकी टोकरी खाली है"));
    assert_eq!(hi["cart.checkout"], json!("Checkout"), "a word it lacks is the English one");
    assert_eq!(hi.len(), en.len(), "every word is there");
    let (lang, dir, _) = words_of(&page_in(&t, "/cart", &[("lang", "ar")], Some("hi")).await.2);
    assert_eq!((lang.as_str(), dir.as_str()), ("ar", "rtl"), "the visitor's own choice comes before the browser's, and Arabic reads right to left");
    let (lang, _, _) = words_of(&page_in(&t, "/", &[], Some("fr;q=1, de;q=0.8")).await.2);
    assert_eq!(lang, "en", "a language the store does not offer: its default");
    let (lang, _, _) = words_of(&page_in(&t, "/", &[], None).await.2);
    assert_eq!(lang, "en");

    // ───────────── the workspace changes the English too ─────────────
    t.insert("StorefrontWords", json!({"language": "en", "words": {"cart.checkout": "Pay now"}})).await;
    let (_, _, en2) = words_of(&page_in(&t, "/cart", &[], None).await.2);
    assert_eq!((en2["cart.checkout"].clone(), en2["cart.empty"].clone()), (json!("Pay now"), json!("Your cart is empty")));
    let (_, _, hi2) = words_of(&page_in(&t, "/cart", &[], Some("hi")).await.2);
    assert_eq!(hi2["cart.checkout"], json!("Checkout"), "one language's words are not another's");
    // Offered languages are the settings': one the store does not list is not served though it has words.
    t.insert("StorefrontWords", json!({"language": "ta", "words": {"cart.empty": "காலி"}})).await;
    assert_eq!(words_of(&page_in(&t, "/", &[("lang", "ta")], None).await.2).0, "en");

    // ───────────── what is refused ─────────────
    let typo = t.k.insert(&t.ctx, "StorefrontWords", obj(json!({"language": "de", "words": {"cart.emtpy": "Leer"}}))).await;
    assert!(matches!(&typo, Err(Error::Validation(f)) if f.iter().any(|x| x.field == "words" && x.message.contains("cart.emtpy"))), "{typo:?}");
    let place = t.k.insert(&t.ctx, "StorefrontWords", obj(json!({"language": "de", "words": {"cart.count_aria": "{count} im Warenkorb"}}))).await;
    assert!(matches!(&place, Err(Error::Validation(f)) if f.iter().any(|x| x.message.contains("{count}"))), "{place:?}");
    let code = t.k.insert(&t.ctx, "StorefrontWords", obj(json!({"language": "en_US", "words": {}}))).await;
    assert!(matches!(&code, Err(Error::Validation(f)) if f.iter().any(|x| x.field == "language")), "{code:?}");
    let twice = t.k.insert(&t.ctx, "StorefrontWords", obj(json!({"language": "hi", "words": {}}))).await;
    assert!(twice.is_err(), "one record a language");
}
