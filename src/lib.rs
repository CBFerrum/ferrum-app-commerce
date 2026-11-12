//! Online store on the same items, prices, stock and books as the rest of Ferrum.
//!
//! * Catalog: `ItemCategory` (a tree), `WebsiteItem` (the published face of an `Item`) and
//!   moderated `ItemReview`s.
//! * Carts (`ShoppingCart`) are sales-shaped documents priced by `ferrum_app_erp::pricing` on every
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
mod edit;
mod gift_card;
mod orders;
mod pages;
mod payment;
mod refund;
mod reports;
mod ship;
mod shipping;
mod shopper;
mod site;
mod store;
mod subscriptions;
mod tell;
mod token;
mod upkeep;
mod variants;

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
        .register("PaymentException", Arc::new(payment::PaymentExceptionHook))
        .register("OrderRefund", Arc::new(refund::OrderRefundHook))
        .register("ReturnRequest", Arc::new(shopper::ReturnRequestHook))
        .register("DeliveryNote", Arc::new(ship::DeliveryHook))
        .register("SalesOrder", Arc::new(payment::WebOrderHook))
        .register("SalesInvoice", Arc::new(payment::AdvanceHook))
        .register("PaymentEntry", Arc::new(payment::PaymentEntryHook));
    // The connector with Finance: only where Store Subscriptions is installed.
    hooks.scoped(&[subscriptions::PACKAGE.to_string()], |h| {
        h.register("SalesOrder", Arc::new(subscriptions::OrderHook)).register("SalesInvoice", Arc::new(subscriptions::RenewalHook));
    });
    hooks.add_site("commerce", Arc::new(site::Storefront));
}

/// The manifest of the connector app with Finance (`connectors/subscriptions/app.yaml`): its package
/// is `commerce_subscriptions`, its code this crate's. It ships with Commerce.
pub const CONNECTOR_MANIFEST: &str = include_str!("../connectors/subscriptions/app.yaml");

/// Asks for `part` of a submitted invoice (a booking's deposit) or all of it through the store's
/// payment method `gateway`, for apps that take payments through the store; `None` when nothing is
/// outstanding. A gateway plugin collects it as it collects an order's.
pub async fn request_invoice_payment(
    k: &Kernel,
    ctx: &Ctx,
    tx: &mut ferrum_kernel::Tx,
    invoice: &ferrum_kernel::Doc,
    gateway: &str,
    part: Option<rust_decimal::Decimal>,
    email: Option<String>,
) -> Result<Option<ferrum_kernel::Doc>> {
    let store = store::Store::load(k, ctx, tx).await?;
    let method = store.method(gateway).cloned().ok_or_else(|| ferrum_kernel::Error::field("payment_method", "is not one of the store's payment methods"))?;
    payment::invoice_request(k, ctx, tx, &store, invoice, &method, part, email, None).await
}

pub use refund::refund_payment;

/// Whether `customer` has a store subscription to `plan` that gives them what it sells (trialling or
/// active), for apps that sell access through the store.
pub async fn entitled(tx: &mut ferrum_kernel::Tx, tenant: uuid::Uuid, customer: uuid::Uuid, plan: uuid::Uuid) -> Result<bool> {
    subscriptions::entitled(tx, tenant, customer, plan).await
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
