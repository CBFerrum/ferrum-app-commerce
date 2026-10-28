//! Store Settings: the one store of a tenant (company, currency, price list, taxes, warehouses,
//! checkout rules and payment methods), read by every storefront request.

use async_trait::async_trait;
use ferrum_domain::util::{b, currency, d, get, id, rows, s, set, sys};
use ferrum_kernel::{Ctx, Doc, DocHook, Error, FieldError, HookCtx, Kernel, Result, Tx};
use rust_decimal::Decimal;
use serde_json::{Value, json};
use std::collections::HashSet;
use uuid::Uuid;

pub const MANUAL: &str = "Manual";
pub const CASH_ON_DELIVERY: &str = "Cash on Delivery";

#[derive(Debug, Clone)]
pub struct PaymentMethod {
    pub gateway: String,
    pub label: String,
    pub account: Option<Uuid>,
    pub expiry_minutes: i64,
    pub instructions: Option<String>,
}

impl PaymentMethod {
    /// Manual and cash-on-delivery requests are settled by staff, not by a gateway plugin.
    pub fn offline(&self) -> bool {
        is_offline(&self.gateway)
    }

    pub fn view(&self) -> Value {
        json!({"gateway": self.gateway, "label": self.label, "instructions": self.instructions, "online": !self.offline()})
    }
}

pub fn is_offline(gateway: &str) -> bool {
    gateway == MANUAL || gateway == CASH_ON_DELIVERY
}

/// A gateway name a payment method may use: a plugin name, Manual or Cash on Delivery.
pub fn valid_gateway(g: &str) -> bool {
    is_offline(g)
        || ((2..=40).contains(&g.len())
            && g.starts_with(|c: char| c.is_ascii_lowercase())
            && g.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'))
}

pub fn valid_country(code: &str) -> bool {
    code.len() == 2 && code.bytes().all(|c| c.is_ascii_uppercase())
}

#[derive(Debug, Clone)]
pub struct Store {
    pub name: String,
    pub company: Uuid,
    pub currency: Uuid,
    pub currency_code: String,
    pub precision: u32,
    pub price_list: Uuid,
    pub tax_template: Option<Uuid>,
    pub display_prices_with_tax: bool,
    /// Warehouses whose stock sells online (resolved: never empty for a company with warehouses).
    pub warehouses: Vec<Uuid>,
    pub fulfilment_warehouse: Uuid,
    pub enforce_stock: bool,
    pub show_stock_quantity: bool,
    pub allow_guest_checkout: bool,
    pub default_customer_group: Option<Uuid>,
    pub shipping_item: Option<Uuid>,
    pub countries: Vec<(String, Option<String>)>,
    pub payment_methods: Vec<PaymentMethod>,
    pub terms_url: Option<String>,
    pub store_url: Option<String>,
    pub max_qty_per_line: Decimal,
    pub max_cart_lines: usize,
    pub abandon_after_hours: i64,
    pub cart_expiry_days: i64,
    pub cancel_unpaid_orders: bool,
}

impl Store {
    /// The tenant's store; `NotFound` until Store Settings exist (the storefront is then closed).
    pub async fn load(k: &Kernel, ctx: &Ctx, tx: &mut Tx) -> Result<Store> {
        let sctx = sys(ctx);
        let found = k.find_tx(&sctx, tx, "StoreSettings", &[], None, 1).await?;
        let sid = found.first().and_then(|r| id(r, "id")).ok_or_else(|| Error::NotFound("store".into()))?;
        // Lists carry no child rows: read the settings whole.
        let doc = get(k, ctx, tx, "StoreSettings", sid).await?;
        let company = id(&doc, "company").ok_or_else(|| Error::NotFound("store".into()))?;
        let cur = id(&doc, "currency").ok_or_else(|| Error::NotFound("store".into()))?;
        let (currency_code, precision) = currency(k, ctx, tx, cur).await?;
        let mut warehouses: Vec<Uuid> = rows(&doc, "warehouses").iter().filter_map(|r| id(r, "warehouse")).collect();
        if warehouses.is_empty() {
            warehouses = sqlx::query_scalar(
                "select id from e_warehouse where tenant_id = $1 and company = $2 and not coalesce(is_group, false) and not coalesce(disabled, false)",
            )
            .bind(ctx.tenant)
            .bind(company)
            .fetch_all(&mut **tx)
            .await?;
        }
        let payment_methods = rows(&doc, "payment_methods")
            .iter()
            .filter(|r| b(r, "enabled"))
            .filter_map(|r| {
                Some(PaymentMethod {
                    gateway: s(r, "gateway")?.to_string(),
                    label: s(r, "label").unwrap_or_else(|| s(r, "gateway").unwrap_or_default()).to_string(),
                    account: id(r, "payment_account"),
                    expiry_minutes: r.get("expiry_minutes").and_then(Value::as_i64).unwrap_or(60).max(0),
                    instructions: s(r, "instructions").map(str::to_string),
                })
            })
            .collect();
        let int = |k: &str, default: i64| doc.get(k).and_then(Value::as_i64).filter(|v| *v > 0).unwrap_or(default);
        let max_qty = d(&doc, "max_qty_per_line");
        Ok(Store {
            name: s(&doc, "store_name").unwrap_or_default().to_string(),
            company,
            currency: cur,
            currency_code,
            precision,
            price_list: id(&doc, "price_list").ok_or_else(|| Error::NotFound("store".into()))?,
            tax_template: id(&doc, "tax_template"),
            display_prices_with_tax: b(&doc, "display_prices_with_tax"),
            warehouses,
            fulfilment_warehouse: id(&doc, "fulfilment_warehouse").ok_or_else(|| Error::NotFound("store".into()))?,
            enforce_stock: b(&doc, "enforce_stock"),
            show_stock_quantity: b(&doc, "show_stock_quantity"),
            allow_guest_checkout: b(&doc, "allow_guest_checkout"),
            default_customer_group: id(&doc, "default_customer_group"),
            shipping_item: id(&doc, "shipping_item"),
            countries: rows(&doc, "allowed_countries")
                .iter()
                .filter_map(|r| Some((s(r, "country_code")?.to_string(), s(r, "country_name").map(str::to_string))))
                .collect(),
            payment_methods,
            terms_url: s(&doc, "terms_url").map(str::to_string),
            store_url: s(&doc, "store_url").map(str::to_string),
            max_qty_per_line: if max_qty > Decimal::ZERO { max_qty } else { Decimal::ONE_HUNDRED },
            max_cart_lines: int("max_cart_lines", 50) as usize,
            abandon_after_hours: int("abandon_after_hours", 24),
            cart_expiry_days: int("cart_expiry_days", 30),
            cancel_unpaid_orders: b(&doc, "cancel_unpaid_orders"),
        })
    }

    pub fn method(&self, gateway: &str) -> Option<&PaymentMethod> {
        self.payment_methods.iter().find(|m| m.gateway == gateway)
    }

    pub fn ships_to(&self, country: &str) -> bool {
        self.countries.is_empty() || self.countries.iter().any(|(c, _)| c == country)
    }

    /// What the storefront may know about the store.
    pub fn view(&self) -> Value {
        json!({
            "name": self.name,
            "currency": self.currency_code,
            "currency_precision": self.precision,
            "display_prices_with_tax": self.display_prices_with_tax,
            "show_stock_quantity": self.show_stock_quantity,
            "allow_guest_checkout": self.allow_guest_checkout,
            "terms_url": self.terms_url,
            "countries": self.countries.iter().map(|(c, n)| json!({"code": c, "name": n})).collect::<Vec<_>>(),
            "payment_methods": self.payment_methods.iter().map(PaymentMethod::view).collect::<Vec<_>>(),
            "max_qty_per_line": self.max_qty_per_line.normalize().to_string(),
            "max_cart_lines": self.max_cart_lines,
        })
    }
}

pub struct SettingsHook;

#[async_trait]
impl DocHook for SettingsHook {
    async fn before_validate(&self, _hc: &mut HookCtx<'_>, doc: &mut Doc) -> Result<()> {
        let mut seen = HashSet::new();
        let countries: Vec<Value> = rows(doc, "allowed_countries")
            .into_iter()
            .filter_map(|mut r| {
                let code = s(&r, "country_code")?.trim().to_uppercase();
                set(&mut r, "country_code", code.clone());
                seen.insert(code).then_some(Value::Object(r))
            })
            .collect();
        set(doc, "allowed_countries", Value::Array(countries));
        Ok(())
    }

    async fn validate(&self, hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        let sctx = sys(hc.ctx);
        let (k, tx) = (hc.kernel, &mut *hc.tx);
        let Some(company) = id(doc, "company") else { return Ok(()) };
        let co = ferrum_domain::util::company(k, &sctx, tx, company).await?;
        let mut errors = Vec::new();
        if let Some(pl) = id(doc, "price_list") {
            let list = get(k, &sctx, tx, "PriceList", pl).await?;
            if !b(&list, "enabled") {
                errors.push(FieldError::new("price_list", "this price list is disabled"));
            }
            // The pricing engine reads price lists as company-currency prices and converts them.
            if id(doc, "currency") != Some(co.currency_id) && id(&list, "currency").is_some_and(|c| c != co.currency_id) {
                errors.push(FieldError::new(
                    "price_list",
                    format!("a store selling in another currency needs a price list in the company currency ({})", co.currency_code),
                ));
            }
        }
        if let Some(t) = id(doc, "tax_template") {
            let tpl = get(k, &sctx, tx, "TaxTemplate", t).await?;
            if id(&tpl, "company") != Some(company) || s(&tpl, "kind") != Some("Sales") {
                errors.push(FieldError::new("tax_template", "choose a sales tax template of the store's company"));
            }
        }
        let listed: Vec<Uuid> = rows(doc, "warehouses").iter().filter_map(|r| id(r, "warehouse")).collect();
        for (i, w) in listed.iter().enumerate() {
            if id(&get(k, &sctx, tx, "Warehouse", *w).await?, "company") != Some(company) {
                errors.push(FieldError::new("warehouse", "belongs to another company").in_row("warehouses", i));
            }
        }
        if let Some(w) = id(doc, "fulfilment_warehouse") {
            if id(&get(k, &sctx, tx, "Warehouse", w).await?, "company") != Some(company) {
                errors.push(FieldError::new("fulfilment_warehouse", "belongs to another company"));
            } else if !listed.is_empty() && !listed.contains(&w) {
                errors.push(FieldError::new("fulfilment_warehouse", "must be one of the store's warehouses"));
            }
        }
        if let Some(item) = id(doc, "shipping_item") {
            let it = get(k, &sctx, tx, "Item", item).await?;
            if b(&it, "is_stock_item") || b(&it, "disabled") {
                errors.push(FieldError::new("shipping_item", "use an enabled non-stock (service) item"));
            }
        }
        for (i, r) in rows(doc, "allowed_countries").iter().enumerate() {
            if !s(r, "country_code").is_some_and(valid_country) {
                errors.push(FieldError::new("country_code", "use a two-letter ISO country code").in_row("allowed_countries", i));
            }
        }
        let mut gateways = HashSet::new();
        for (i, r) in rows(doc, "payment_methods").iter().enumerate() {
            let g = s(r, "gateway").unwrap_or_default();
            if !valid_gateway(g) {
                errors.push(FieldError::new("gateway", format!("use a gateway plugin's name, {MANUAL} or {CASH_ON_DELIVERY}")).in_row("payment_methods", i));
            } else if !gateways.insert(g.to_string()) {
                errors.push(FieldError::new("gateway", "listed twice").in_row("payment_methods", i));
            }
            if let Some(acc) = id(r, "payment_account") {
                let a = get(k, &sctx, tx, "Account", acc).await?;
                if id(&a, "company") != Some(company) || !matches!(s(&a, "account_type"), Some("Bank" | "Cash")) {
                    errors.push(FieldError::new("payment_account", "choose a bank or cash account of the store's company").in_row("payment_methods", i));
                }
            }
        }
        if errors.is_empty() {
            return Ok(());
        }
        Err(Error::Validation(errors))
    }
}

/// Website Managers, who hear about new web orders and payments.
pub async fn managers(tx: &mut Tx, tenant: Uuid) -> Result<Vec<Uuid>> {
    Ok(sqlx::query_scalar(
        "select u.id from app_user u join user_role r on r.tenant_id = u.tenant_id and r.user_id = u.id \
         where u.tenant_id = $1 and r.role = 'Website Manager' and u.is_active order by u.created_at limit 25",
    )
    .bind(tenant)
    .fetch_all(&mut **tx)
    .await?)
}

/// Notifies the store's managers in the caller's transaction (delivered at commit).
pub async fn notify_managers(tx: &mut Tx, tenant: Uuid, title: &str, body: Option<&str>, entity: &str, record: Uuid) -> Result<()> {
    for user in managers(tx, tenant).await? {
        ferrum_kernel::notify::send(tx, tenant, user, title, body, Some(entity), Some(record)).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gateway_and_country_names() {
        for ok in ["Manual", "Cash on Delivery", "stripe", "razorpay-in", "pay2"] {
            assert!(valid_gateway(ok), "{ok}");
        }
        for bad in ["", "x", "Stripe", "pay pal", "-x", "cash on delivery", &"a".repeat(41)] {
            assert!(!valid_gateway(bad), "{bad}");
        }
        assert!(valid_country("IN") && !valid_country("in") && !valid_country("IND") && !valid_country("I1"));
        assert!(is_offline(MANUAL) && is_offline(CASH_ON_DELIVERY) && !is_offline("stripe"));
    }
}
