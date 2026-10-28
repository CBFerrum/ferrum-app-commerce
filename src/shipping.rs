//! Shipping rules: a fixed charge, or bands over the cart subtotal or its weight, limited to a
//! list of countries. The charge becomes a service line on the cart and the order, so it is
//! taxed, invoiced and booked like any other line.

use crate::store::valid_country;
use async_trait::async_trait;
use ferrum_domain::util::{b, d, find, get, id, rows, s, set};
use ferrum_kernel::{Ctx, Doc, DocHook, Error, FieldError, Filter, HookCtx, Kernel, Result, Tx};
use rust_decimal::Decimal;
use serde_json::{Value, json};
use std::collections::HashSet;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Basis {
    Fixed,
    OrderAmount,
    TotalWeight,
}

#[derive(Debug, Clone)]
pub struct Rule {
    pub id: Uuid,
    pub label: String,
    pub description: Option<String>,
    pub basis: Basis,
    pub amount: Decimal,
    /// `(from inclusive, to exclusive or 0 = open, charge)`.
    pub bands: Vec<(Decimal, Decimal, Decimal)>,
    pub countries: Vec<String>,
}

impl Rule {
    fn from_doc(doc: &Doc) -> Option<Rule> {
        Some(Rule {
            id: id(doc, "id")?,
            label: s(doc, "label").unwrap_or_default().to_string(),
            description: s(doc, "description").map(str::to_string),
            basis: match s(doc, "basis") {
                Some("Order Amount") => Basis::OrderAmount,
                Some("Total Weight") => Basis::TotalWeight,
                _ => Basis::Fixed,
            },
            amount: d(doc, "amount"),
            bands: rows(doc, "bands").iter().map(|r| (d(r, "from_value"), d(r, "to_value"), d(r, "amount"))).collect(),
            countries: rows(doc, "countries").iter().filter_map(|r| s(r, "country_code").map(str::to_string)).collect(),
        })
    }

    /// The charge for a destination, cart subtotal and weight; `None` when the rule does not apply.
    pub fn charge(&self, country: Option<&str>, subtotal: Decimal, weight: Decimal) -> Option<Decimal> {
        if !self.countries.is_empty() && !country.is_some_and(|c| self.countries.iter().any(|x| x == c)) {
            return None;
        }
        let measure = match self.basis {
            Basis::Fixed => return Some(self.amount),
            Basis::OrderAmount => subtotal,
            Basis::TotalWeight => weight,
        };
        self.bands.iter().find(|(from, to, _)| measure >= *from && (to.is_zero() || measure < *to)).map(|(_, _, a)| *a)
    }

    pub fn view(&self, charge: Decimal) -> Value {
        json!({"id": self.id, "label": self.label, "description": self.description, "amount": charge.normalize().to_string()})
    }
}

/// Enabled rules in display order (with their bands and countries).
pub async fn enabled_rules(k: &Kernel, ctx: &Ctx, tx: &mut Tx) -> Result<Vec<Rule>> {
    let listed = find(k, ctx, tx, "ShippingRule", vec![Filter::eq("enabled", true)], Some("sort_order asc"), 100).await?;
    let mut out = Vec::with_capacity(listed.len());
    for r in listed {
        let Some(rid) = id(&r, "id") else { continue };
        // Lists carry no child rows: read each rule whole.
        if let Some(rule) = Rule::from_doc(&get(k, ctx, tx, "ShippingRule", rid).await?) {
            out.push(rule);
        }
    }
    Ok(out)
}

/// Problems with a rule's bands: overlaps, empty ranges, an open band before the last.
fn band_errors(bands: &[(Decimal, Decimal, Decimal)]) -> Vec<(usize, &'static str)> {
    let mut order: Vec<usize> = (0..bands.len()).collect();
    order.sort_by(|a, b| bands[*a].0.cmp(&bands[*b].0));
    let mut errors = Vec::new();
    let mut previous_end: Option<Decimal> = None;
    let mut open_seen = false;
    for i in order {
        let (from, to, _) = bands[i];
        if !to.is_zero() && to <= from {
            errors.push((i, "to value must be above the from value (or 0 for no limit)"));
            continue;
        }
        if open_seen || previous_end.is_some_and(|end| from < end) {
            errors.push((i, "overlaps another band"));
        }
        if to.is_zero() {
            open_seen = true;
        } else {
            previous_end = Some(previous_end.map_or(to, |e| e.max(to)));
        }
    }
    errors
}

pub struct ShippingRuleHook;

#[async_trait]
impl DocHook for ShippingRuleHook {
    async fn before_validate(&self, _hc: &mut HookCtx<'_>, doc: &mut Doc) -> Result<()> {
        let mut seen = HashSet::new();
        let countries: Vec<Value> = rows(doc, "countries")
            .into_iter()
            .filter_map(|mut r| {
                let code = s(&r, "country_code")?.trim().to_uppercase();
                set(&mut r, "country_code", code.clone());
                seen.insert(code).then_some(Value::Object(r))
            })
            .collect();
        set(doc, "countries", Value::Array(countries));
        Ok(())
    }

    async fn validate(&self, hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        let Some(rule) = Rule::from_doc(doc) else { return Ok(()) };
        let mut errors = Vec::new();
        if rule.basis != Basis::Fixed && rule.bands.is_empty() {
            errors.push(FieldError::new("bands", "add at least one band"));
        }
        if rule.basis != Basis::Fixed {
            errors.extend(band_errors(&rule.bands).into_iter().map(|(i, m)| FieldError::new("from_value", m).in_row("bands", i)));
        }
        for (i, r) in rows(doc, "countries").iter().enumerate() {
            if !s(r, "country_code").is_some_and(valid_country) {
                errors.push(FieldError::new("country_code", "use a two-letter ISO country code").in_row("countries", i));
            }
        }
        if !errors.is_empty() {
            return Err(Error::Validation(errors));
        }
        let charges = rule.amount > Decimal::ZERO || rule.bands.iter().any(|(_, _, a)| *a > Decimal::ZERO);
        if charges && b(doc, "enabled") && crate::installed(hc.ctx) {
            let sctx = ferrum_domain::util::sys(hc.ctx);
            let settings = find(hc.kernel, &sctx, hc.tx, "StoreSettings", vec![], None, 1).await?;
            if settings.first().is_some_and(|st| id(st, "shipping_item").is_none()) {
                return Err(Error::field("amount", "set the Shipping Item in Store Settings first: shipping charges are billed with it"));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn n(s: &str) -> Decimal {
        Decimal::from_str(s).unwrap()
    }

    fn rule(basis: Basis, bands: &[(&str, &str, &str)], countries: &[&str]) -> Rule {
        Rule {
            id: Uuid::now_v7(),
            label: "Standard".into(),
            description: None,
            basis,
            amount: n("50"),
            bands: bands.iter().map(|(a, b, c)| (n(a), n(b), n(c))).collect(),
            countries: countries.iter().map(|c| c.to_string()).collect(),
        }
    }

    #[test]
    fn bands_are_half_open_and_countries_restrict() {
        let by_amount = rule(Basis::OrderAmount, &[("0", "500", "60"), ("500", "0", "0")], &[]);
        assert_eq!(by_amount.charge(None, n("499.99"), Decimal::ZERO), Some(n("60")));
        assert_eq!(by_amount.charge(None, n("500"), Decimal::ZERO), Some(Decimal::ZERO), "free from 500 up");
        let by_weight = rule(Basis::TotalWeight, &[("0", "2", "40"), ("2", "10", "90")], &["IN"]);
        assert_eq!(by_weight.charge(Some("IN"), Decimal::ZERO, n("1.5")), Some(n("40")));
        assert_eq!(by_weight.charge(Some("IN"), Decimal::ZERO, n("2")), Some(n("90")));
        assert_eq!(by_weight.charge(Some("IN"), Decimal::ZERO, n("10")), None, "heavier than every band");
        assert_eq!(by_weight.charge(Some("DE"), Decimal::ZERO, n("1")), None, "not shipped there");
        assert_eq!(by_weight.charge(None, Decimal::ZERO, n("1")), None, "destination unknown");
        assert_eq!(rule(Basis::Fixed, &[], &[]).charge(Some("US"), Decimal::ZERO, Decimal::ZERO), Some(n("50")));
    }

    #[test]
    fn band_validation() {
        let b = |v: &[(&str, &str)]| v.iter().map(|(a, c)| (n(a), n(c), Decimal::ONE)).collect::<Vec<_>>();
        assert!(band_errors(&b(&[("0", "500"), ("500", "0")])).is_empty());
        assert!(band_errors(&b(&[("500", "0"), ("0", "500")])).is_empty(), "order does not matter");
        assert_eq!(band_errors(&b(&[("0", "500"), ("400", "0")])).len(), 1, "overlap");
        assert_eq!(band_errors(&b(&[("0", "0"), ("100", "200")])).len(), 1, "an open band swallows the rest");
        assert_eq!(band_errors(&b(&[("10", "5")])).len(), 1, "empty range");
    }
}
