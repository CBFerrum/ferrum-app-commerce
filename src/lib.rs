//! Online store on the same items, prices, stock and books as the rest of Ferrum.
//!
//! * Catalog: `ItemCategory` (a tree), `WebsiteItem` (the published face of an `Item`) and
//!   moderated `ItemReview`s.
//! * Carts (`ShoppingCart`) are sales-shaped documents priced by `ferrum_domain::pricing` on every
//!   change, so a cart, the sales order made from it and that order's invoice agree to the cent.
//!   Shipping is a service line priced by a `ShippingRule`; a `CouponCode` unlocks a pricing rule.
//! * Checkout turns a cart into a submitted `SalesOrder` and a `PaymentRequest`. Gateway plugins
//!   confirm payments through the request's `mark_paid` action, which books a `PaymentEntry` (an
//!   advance on the order, applied to its invoice when that is submitted).
//! * The storefront is a `Site`: the public at `/api/site/{tenant}/commerce/...`, signed-in
//!   customers at `/api/app/commerce/...` — see `packages/commerce/README.md`.

mod cart;
mod catalog;
mod checkout;
mod coupon;
mod orders;
mod payment;
mod reports;
mod shipping;
mod site;
mod store;
mod token;
mod upkeep;

use chrono::{DateTime, NaiveDate, SecondsFormat, Utc};
use ferrum_domain::reports::ReportSource;
use ferrum_kernel::{Ctx, HookRegistry, Kernel, Result};
use std::sync::Arc;

/// Registers this package's document hooks and its storefront. The hooks on other packages'
/// entities (orders, invoices, payments, pricing rules) do nothing for tenants without commerce.
pub fn register(hooks: &mut HookRegistry) {
    hooks
        .register("StoreSettings", Arc::new(store::SettingsHook))
        .register("ItemCategory", Arc::new(catalog::CategoryHook))
        .register("WebsiteItem", Arc::new(catalog::WebsiteItemHook))
        .register("ItemReview", Arc::new(catalog::ReviewHook))
        .register("ShoppingCart", Arc::new(cart::CartHook))
        .register("ShippingRule", Arc::new(shipping::ShippingRuleHook))
        .register("CouponCode", Arc::new(coupon::CouponHook))
        .register("PricingRule", Arc::new(coupon::PricingRuleGuard))
        .register("PaymentRequest", Arc::new(payment::PaymentRequestHook))
        .register("SalesOrder", Arc::new(payment::WebOrderHook))
        .register("SalesInvoice", Arc::new(payment::AdvanceHook))
        .register("PaymentEntry", Arc::new(payment::PaymentEntryHook));
    hooks.add_site("commerce", Arc::new(site::Storefront));
}

/// This package's reports.
pub fn reports() -> Vec<Arc<dyn ReportSource>> {
    vec![Arc::new(reports::CommerceReports)]
}

/// Periodic upkeep: every minute, on one replica at a time (advisory lock); must be idempotent and cheap.
/// Expires unpaid payment requests (cancelling their orders to release stock when the store says
/// so), marks idle carts abandoned and old ones expired.
pub async fn maintenance(k: &Kernel) -> Result<()> {
    upkeep::run(k).await
}

/// Whether the tenant behind `ctx` installed this package (hooks on shared entities check it).
pub(crate) fn installed(ctx: &Ctx) -> bool {
    ctx.schema.packages.iter().any(|p| p == "commerce")
}

pub(crate) fn now() -> DateTime<Utc> {
    Utc::now()
}

pub(crate) fn now_str() -> String {
    now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

pub(crate) fn today() -> NaiveDate {
    now().date_naive()
}

include!(concat!(env!("OUT_DIR"), "/packages.rs"));
include!(concat!(env!("OUT_DIR"), "/ui.rs"));

/// This app's manifest (`app.yaml`).
pub const MANIFEST: &str = include_str!("../app.yaml");

/// This app's metadata packages, embedded at build time.
pub fn packages() -> std::result::Result<Vec<ferrum_meta::Package>, ferrum_meta::MetaError> {
    ferrum_meta::catalog::load_embedded(EMBEDDED)
}

#[cfg(test)]
mod interface_tests {
    use super::*;

    #[test]
    fn the_interface_is_a_release_the_platform_takes() {
        let m = ferrum_apps::AppManifest::parse(MANIFEST).expect("app.yaml");
        assert_eq!(m.routes(), vec![("commerce".to_string(), vec![])]);
        let files = UI.iter().map(|(p, b)| (p.to_string(), b.to_vec())).collect();
        let ui = ferrum_apps::ui::prepare("commerce", files).expect("ui/dist is an interface release");
        assert!(UI.iter().any(|(p, _)| *p == "index.html") && !UI.iter().any(|(p, _)| p.starts_with('.')), "{}", ui.digest);
    }
}
