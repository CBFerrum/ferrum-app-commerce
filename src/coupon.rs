//! Coupon codes. A coupon unlocks a pricing rule: saving the coupon puts
//! `doc.coupon_rule == '<rule id>'` into the rule's condition, which only a cart carrying a
//! validated coupon satisfies (`coupon_rule` is set by the server, never by the client), so the
//! rule's discount reaches nothing else — not staff orders, not other carts. The guard on
//! `PricingRule` keeps that gate in place while coupons use the rule.

use async_trait::async_trait;
use ferrum_domain::util::{b, d, find, get, id, s, set, sys};
use ferrum_kernel::{Ctx, Doc, DocHook, Error, Filter, HookCtx, Kernel, Result, Tx};
use rust_decimal::Decimal;
use serde_json::json;
use uuid::Uuid;

pub fn gate(rule: Uuid) -> String {
    format!("doc.coupon_rule == '{rule}'")
}

pub fn normalize_code(raw: &str) -> String {
    raw.trim().to_uppercase()
}

pub fn valid_code(code: &str) -> bool {
    (3..=64).contains(&code.len()) && code.bytes().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == b'-' || c == b'_')
}

/// The rule condition with the coupon gate in front of whatever the rule already required.
fn gated_condition(rule: Uuid, existing: Option<&str>) -> Option<String> {
    let g = gate(rule);
    match existing.map(str::trim).filter(|c| !c.is_empty()) {
        Some(c) if c.contains(&g) => None,
        Some(c) => Some(format!("{g} && ({c})")),
        None => Some(g),
    }
}

pub struct CouponHook;

#[async_trait]
impl DocHook for CouponHook {
    async fn before_validate(&self, _hc: &mut HookCtx<'_>, doc: &mut Doc) -> Result<()> {
        if let Some(code) = s(doc, "code") {
            let c = normalize_code(code);
            set(doc, "code", c);
        }
        Ok(())
    }

    async fn validate(&self, _hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        if !s(doc, "code").is_some_and(valid_code) {
            return Err(Error::field("code", "use 3-64 letters, digits, '-' or '_'"));
        }
        Ok(())
    }

    async fn after_save(&self, hc: &mut HookCtx<'_>, doc: &Doc, _old: Option<&Doc>) -> Result<()> {
        let Some(rule) = id(doc, "pricing_rule") else { return Ok(()) };
        let sctx = sys(hc.ctx);
        let current = get(hc.kernel, &sctx, hc.tx, "PricingRule", rule).await?;
        if let Some(condition) = gated_condition(rule, s(&current, "condition")) {
            let mut patch = Doc::new();
            patch.insert("condition".into(), json!(condition));
            hc.kernel.update_tx(&sctx, hc.tx, "PricingRule", rule, patch, None).await?;
        }
        Ok(())
    }
}

pub struct PricingRuleGuard;

#[async_trait]
impl DocHook for PricingRuleGuard {
    async fn validate(&self, hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        let Some(rule) = id(doc, "id") else { return Ok(()) };
        if !crate::installed(hc.ctx) || s(doc, "condition").is_some_and(|c| c.contains(&gate(rule))) {
            return Ok(());
        }
        let used = find(hc.kernel, hc.ctx, hc.tx, "CouponCode", vec![Filter::eq("pricing_rule", rule.to_string())], None, 1).await?;
        if used.is_empty() {
            return Ok(());
        }
        Err(Error::field("condition", format!("coupon codes unlock this rule: keep `{}` in its condition", gate(rule))))
    }
}

pub async fn by_code(k: &Kernel, ctx: &Ctx, tx: &mut Tx, code: &str) -> Result<Option<Doc>> {
    Ok(find(k, ctx, tx, "CouponCode", vec![Filter::eq("code", normalize_code(code))], None, 1).await?.into_iter().next())
}

fn refused(message: impl Into<String>) -> Error {
    Error::field("coupon_code", message)
}

/// Refuses a coupon that is disabled, out of its dates, used up overall or by this customer or
/// email address, or above the cart's goods total at list prices.
#[allow(clippy::too_many_arguments)]
pub async fn check(
    tx: &mut Tx,
    tenant: Uuid,
    coupon: &Doc,
    list_subtotal: Decimal,
    customer: Option<Uuid>,
    email: Option<&str>,
    cart: Uuid,
    today: &str,
) -> Result<()> {
    if b(coupon, "disabled") {
        return Err(refused("this coupon is no longer valid"));
    }
    if s(coupon, "valid_from").is_some_and(|f| f > today) {
        return Err(refused("this coupon is not valid yet"));
    }
    if s(coupon, "valid_upto").is_some_and(|u| u < today) {
        return Err(refused("this coupon has expired"));
    }
    let max = coupon.get("max_uses").and_then(serde_json::Value::as_i64).unwrap_or(0);
    if max > 0 && coupon.get("used_count").and_then(serde_json::Value::as_i64).unwrap_or(0) >= max {
        return Err(refused("this coupon has been used up"));
    }
    let min = d(coupon, "min_order_amount");
    if min > Decimal::ZERO && list_subtotal < min {
        return Err(refused(format!("spend at least {} to use this coupon", min.normalize())));
    }
    let per_customer = coupon.get("max_uses_per_customer").and_then(serde_json::Value::as_i64).unwrap_or(0);
    if per_customer > 0 && (customer.is_some() || email.is_some()) {
        let used: i64 = sqlx::query_scalar(
            "select count(*) from e_shopping_cart c join e_sales_order o on o.tenant_id = c.tenant_id and o.id = c.sales_order \
             where c.tenant_id = $1 and c.coupon = $2 and c.status = 'Ordered' and o.docstatus = 1 and c.id <> $3 \
             and (c.customer = $4 or ($5::text is not null and c.email = $5))",
        )
        .bind(tenant)
        .bind(id(coupon, "id"))
        .bind(cart)
        .bind(customer)
        .bind(email.map(str::to_lowercase))
        .fetch_one(&mut **tx)
        .await?;
        if used >= per_customer {
            return Err(refused("you have already used this coupon"));
        }
    }
    Ok(())
}

/// Counts one use, atomically refusing to go past the coupon's limit.
pub async fn redeem(tx: &mut Tx, tenant: Uuid, coupon: Uuid) -> Result<()> {
    let n = sqlx::query(
        "update e_coupon_code set used_count = coalesce(used_count, 0) + 1, updated_at = now() \
         where tenant_id = $1 and id = $2 and (coalesce(max_uses, 0) = 0 or coalesce(used_count, 0) < max_uses)",
    )
    .bind(tenant)
    .bind(coupon)
    .execute(&mut **tx)
    .await?
    .rows_affected();
    if n == 0 {
        return Err(refused("this coupon has been used up"));
    }
    Ok(())
}

/// Gives a use back (the order using it was cancelled).
pub async fn release(tx: &mut Tx, tenant: Uuid, coupon: Uuid) -> Result<()> {
    sqlx::query("update e_coupon_code set used_count = greatest(coalesce(used_count, 0) - 1, 0), updated_at = now() where tenant_id = $1 and id = $2")
        .bind(tenant)
        .bind(coupon)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes() {
        assert_eq!(normalize_code("  summer-10 "), "SUMMER-10");
        assert!(valid_code("SUMMER_10") && valid_code("ABC"));
        assert!(!valid_code("AB") && !valid_code("SUM MER") && !valid_code("summer") && !valid_code(&"A".repeat(65)));
    }

    #[test]
    fn rules_get_gated_once() {
        let r = Uuid::now_v7();
        assert_eq!(gated_condition(r, None), Some(gate(r)));
        assert_eq!(gated_condition(r, Some("  ")), Some(gate(r)));
        let combined = gated_condition(r, Some("doc.item_count > 2.0")).unwrap();
        assert_eq!(combined, format!("{} && (doc.item_count > 2.0)", gate(r)));
        assert_eq!(gated_condition(r, Some(&combined)), None, "already gated");
        ferrum_meta::Expr::compile(&combined).expect("valid CEL");
    }
}
