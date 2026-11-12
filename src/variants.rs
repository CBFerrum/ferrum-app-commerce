//! A product with options (size, colour) whose combinations are ERP's item variants.
//!
//! ERP models variants in its Stock Operations add-on: item attributes (Size, with its values), a template
//! item that varies by some of them, and `make_variants`, which creates the missing combinations as items of
//! their own (their own code, price and stock). The store adds nothing to that model: "Generate Variants" on
//! a website item makes (or completes) the attributes, marks the item's template, has ERP make the
//! combinations, and gives each its own website item (a variant of this one, with the options it differs by,
//! which is what the product page's option picker reads) and a price in the store's price list (the
//! template's, to be changed per combination). A value the store disables is never sold: the combinations
//! that have it are made unpublished and cannot be published.

use crate::store::Store;
use ferrum_domain::util::{b, elevated, find, get, id, money, rows, s, set};
use ferrum_kernel::{Doc, Error, Filter, HookCtx, Result};
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use uuid::Uuid;

/// Most options a product varies by, values to an option, and combinations made at once.
const MAX_OPTIONS: usize = 3;
const MAX_VALUES: usize = 30;
const MAX_COMBINATIONS: usize = 100;

/// `{"Size": ["S", "M"], "Colour": ["Red"]}` as given (an object or its JSON text), tidied: names and
/// values trimmed, a value once.
pub(crate) fn parse_options(raw: Option<&Value>, field: &str) -> Result<BTreeMap<String, Vec<String>>> {
    let value = match raw {
        None | Some(Value::Null) => return Ok(BTreeMap::new()),
        Some(Value::String(t)) if t.trim().is_empty() => return Ok(BTreeMap::new()),
        Some(Value::String(t)) => serde_json::from_str::<Value>(t).map_err(|_| Error::field(field, "is not a JSON object like {\"Size\": [\"S\", \"M\"]}"))?,
        Some(v) => v.clone(),
    };
    let Value::Object(map) = value else { return Err(Error::field(field, "must be an object like {\"Size\": [\"S\", \"M\"]}")) };
    let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (name, values) in map {
        let Value::Array(list) = values else { return Err(Error::field(field, format!("{name} needs a list of values"))) };
        let mut vals: Vec<String> = Vec::new();
        for v in list {
            let t = v.as_str().map(str::trim).unwrap_or_default().to_string();
            if t.is_empty() || t.chars().count() > 60 {
                return Err(Error::field(field, format!("each value of {name} is some text of at most 60 characters")));
            }
            if !vals.iter().any(|x| x.eq_ignore_ascii_case(&t)) {
                vals.push(t);
            }
        }
        let name = name.trim().to_string();
        if name.is_empty() || name.chars().count() > 60 || vals.is_empty() || out.keys().any(|k| k.eq_ignore_ascii_case(&name)) {
            return Err(Error::field(field, "each option has a name (once) and some values"));
        }
        if vals.len() > MAX_VALUES {
            return Err(Error::field(field, format!("{name} has more than {MAX_VALUES} values")));
        }
        out.insert(name, vals);
    }
    Ok(out)
}

/// A short code for a value, for the codes of variants: its letters and digits in capitals, at most
/// three; one an earlier value of the attribute already has gets a number.
pub(crate) fn abbreviation(value: &str, taken: &[String]) -> String {
    let base: String = value.chars().filter(char::is_ascii_alphanumeric).take(3).collect::<String>().to_uppercase();
    let base = if base.is_empty() { "V".to_string() } else { base };
    let mut abbr = base.clone();
    let mut n = 2;
    while taken.iter().any(|t| t.eq_ignore_ascii_case(&abbr)) {
        abbr = format!("{base}{n}");
        n += 1;
    }
    abbr
}

/// Whether a variant's options include a value the store has disabled.
pub(crate) fn has_disabled(options: &[(String, String)], disabled: &[(String, String)]) -> bool {
    options.iter().any(|(o, v)| disabled.iter().any(|(do_, dv)| do_.eq_ignore_ascii_case(o) && dv.eq_ignore_ascii_case(v)))
}

/// The (option, value) pairs a product's disabled options list.
pub(crate) fn disabled_of(doc: &Doc) -> Vec<(String, String)> {
    rows(doc, "disabled_options").iter().filter_map(|r| Some((s(r, "option")?.trim().to_string(), s(r, "value")?.trim().to_string()))).collect()
}

fn installed(hc: &HookCtx<'_>) -> bool {
    hc.ctx.schema.entity("ItemAttribute").is_some()
        && hc.ctx.schema.entity("Item").is_some_and(|e| e.field("stk_has_variants").is_some() && e.field("stk_attributes").is_some())
}

/// Finds the attribute called `name` (any case) or makes it, with every value given (and the values it already
/// has); its id.
async fn attribute(hc: &mut HookCtx<'_>, name: &str, values: &[String]) -> Result<Uuid> {
    let actor = elevated(hc.ctx);
    let found = find(hc.kernel, &actor, hc.tx, "ItemAttribute", vec![], None, 500).await?;
    let existing = found.iter().find(|a| s(a, "attribute_name").is_some_and(|n| n.eq_ignore_ascii_case(name))).and_then(|a| id(a, "id"));
    let (attr, mut have): (Uuid, Vec<Doc>) = match existing {
        Some(a) => {
            let doc = get(hc.kernel, &actor, hc.tx, "ItemAttribute", a).await?;
            if b(&doc, "numeric_values") {
                return Err(Error::field("options", format!("{name} is a numeric range in ERP: options are lists of values")));
            }
            (a, rows(&doc, "values"))
        }
        None => {
            // A new attribute is made with its values (ERP wants some).
            let mut made: Vec<Doc> = Vec::new();
            for v in values {
                let taken: Vec<String> = made.iter().filter_map(|r| s(r, "abbr").map(str::to_string)).collect();
                made.push(json!({"attribute_value": v, "abbr": abbreviation(v, &taken)}).as_object().cloned().unwrap_or_default());
            }
            let input = json!({"attribute_name": name, "values": made});
            let doc = hc.kernel.insert_tx(&actor, hc.tx, "ItemAttribute", input.as_object().cloned().unwrap_or_default()).await?;
            return id(&doc, "id").ok_or_else(|| Error::internal("attribute without id"));
        }
    };
    let mut changed = false;
    for v in values {
        if !have.iter().any(|r| s(r, "attribute_value").is_some_and(|x| x.eq_ignore_ascii_case(v))) {
            let taken: Vec<String> = have.iter().filter_map(|r| s(r, "abbr").map(str::to_string)).collect();
            have.push(json!({"attribute_value": v, "abbr": abbreviation(v, &taken)}).as_object().cloned().unwrap_or_default());
            changed = true;
        }
    }
    if changed {
        let mut patch = Doc::new();
        set(&mut patch, "values", Value::Array(have.into_iter().map(Value::Object).collect()));
        hc.kernel.update_tx(&actor, hc.tx, "ItemAttribute", attr, patch, None).await?;
    }
    Ok(attr)
}

/// "Generate Variants" on a website item.
pub async fn generate(hc: &mut HookCtx<'_>, wi: &Doc, params: &Value) -> Result<Value> {
    if !installed(hc) {
        return Err(Error::invalid("item variants are ERP's: install its Stock Operations add-on, then generate them here"));
    }
    if id(wi, "variant_of").is_some() {
        return Err(Error::invalid("this item is a variant itself: generate variants on the item they belong to"));
    }
    let wid = id(wi, "id").ok_or_else(|| Error::internal("website item without id"))?;
    let template = id(wi, "item").ok_or_else(|| Error::internal("website item without item"))?;
    let options = parse_options(params.get("options"), "options")?;
    let disable = parse_options(params.get("disabled"), "disabled")?;
    if options.is_empty() {
        return Err(Error::field("options", "name what it comes in, like {\"Size\": [\"S\", \"M\"], \"Colour\": [\"Red\", \"Blue\"]}"));
    }
    if options.len() > MAX_OPTIONS || options.values().map(Vec::len).product::<usize>() > MAX_COMBINATIONS {
        return Err(Error::field("options", format!("at most {MAX_OPTIONS} options and {MAX_COMBINATIONS} combinations at once")));
    }
    let actor = elevated(hc.ctx);
    let sctx = ferrum_domain::util::sys(hc.ctx);
    let store = Store::load(hc.kernel, &sctx, hc.tx).await?;
    // The store's disabled values, kept on the product: what was disabled before stays so.
    let mut disabled = disabled_of(wi);
    for (name, values) in &disable {
        for v in values {
            if !has_disabled(&[(name.clone(), v.clone())], &disabled) {
                disabled.push((name.clone(), v.clone()));
            }
        }
    }
    let mut patch = Doc::new();
    set(&mut patch, "disabled_options", Value::Array(disabled.iter().map(|(o, v)| json!({"option": o, "value": v})).collect()));
    hc.kernel.update_tx(&sctx, hc.tx, "WebsiteItem", wid, patch, None).await?;

    // ERP's template: the item varies by these attributes (those it already varies by stay).
    let item = get(hc.kernel, &actor, hc.tx, "Item", template).await?;
    if id(&item, "stk_variant_of").is_some() {
        return Err(Error::invalid("this item is itself a variant in ERP: make the variants on its template"));
    }
    let mut attributes = rows(&item, "stk_attributes");
    let mut ids = BTreeMap::new();
    for (name, values) in &options {
        let attr = attribute(hc, name, values).await?;
        ids.insert(name.clone(), attr);
        if !attributes.iter().any(|a| id(a, "attribute") == Some(attr)) {
            attributes.push(json!({"attribute": attr.to_string()}).as_object().cloned().unwrap_or_default());
        }
    }
    let mut patch = Doc::new();
    set(&mut patch, "stk_has_variants", true);
    set(&mut patch, "stk_attributes", Value::Array(attributes.into_iter().map(Value::Object).collect()));
    hc.kernel.update_tx(&actor, hc.tx, "Item", template, patch, None).await?;
    let chosen: Map<String, Value> = options.iter().map(|(n, v)| (n.clone(), json!(v))).collect();
    let made =
        hc.kernel.run_action_tx(&actor, hc.tx, "Item", template, "make_variants", &json!({"attributes": chosen})).await.map_err(crate::refund::for_person)?;

    // Each combination's website item and price.
    let template_rate: Option<rust_decimal::Decimal> =
        sqlx::query_scalar("select rate from e_item_price where tenant_id = $1 and item = $2 and price_list = $3 order by created_at limit 1")
            .bind(hc.ctx.tenant)
            .bind(template)
            .bind(store.price_list)
            .fetch_optional(&mut **hc.tx)
            .await?;
    let variants = find(hc.kernel, &actor, hc.tx, "Item", vec![Filter::eq("stk_variant_of", template.to_string())], None, 500).await?;
    let (mut created, mut unpublished) = (0, 0);
    for v in variants {
        let vid = id(&v, "id").ok_or_else(|| Error::internal("variant without id"))?;
        let doc = get(hc.kernel, &actor, hc.tx, "Item", vid).await?;
        // What sets it apart, by the attributes' names (in the order given).
        let mut pairs: Vec<(String, String)> = Vec::new();
        for r in rows(&doc, "stk_attributes") {
            let (Some(attr), Some(value)) = (id(&r, "attribute"), s(&r, "attribute_value")) else { continue };
            if let Some((name, _)) = ids.iter().find(|(_, a)| **a == attr) {
                pairs.push((name.clone(), value.to_string()));
            }
        }
        // Only combinations of what was asked for (the template may have been varied before).
        if pairs.len() != options.len() || !pairs.iter().all(|(n, val)| options.get(n).is_some_and(|vs| vs.iter().any(|x| x.eq_ignore_ascii_case(val)))) {
            continue;
        }
        let known: Option<Uuid> = sqlx::query_scalar("select id from e_website_item where tenant_id = $1 and item = $2")
            .bind(hc.ctx.tenant)
            .bind(vid)
            .fetch_optional(&mut **hc.tx)
            .await?;
        if known.is_none() {
            let off = has_disabled(&pairs, &disabled);
            let site = json!({"item": vid.to_string(), "variant_of": wid.to_string(), "published": !off,
                              "options": pairs.iter().map(|(o, val)| json!({"option": o, "value": val})).collect::<Vec<_>>()});
            hc.kernel.insert_tx(&sctx, hc.tx, "WebsiteItem", site.as_object().cloned().unwrap_or_default()).await?;
            created += 1;
            unpublished += usize::from(off);
        }
        // Its price in the store's list: the template's until someone sets its own.
        let priced: bool = sqlx::query_scalar("select exists(select 1 from e_item_price where tenant_id = $1 and item = $2 and price_list = $3)")
            .bind(hc.ctx.tenant)
            .bind(vid)
            .bind(store.price_list)
            .fetch_one(&mut **hc.tx)
            .await?;
        if let (false, Some(rate)) = (priced, template_rate) {
            let price = json!({"item": vid.to_string(), "price_list": store.price_list.to_string(), "rate": money(rate)});
            hc.kernel.insert_tx(&actor, hc.tx, "ItemPrice", price.as_object().cloned().unwrap_or_default()).await?;
        }
    }
    Ok(json!({"variants_made_by_erp": made, "website_items_made": created, "unpublished": unpublished}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options_are_names_with_values() {
        let o = parse_options(Some(&json!({"Size": ["S", " M ", "s"], "Colour": "Red"})), "options");
        assert!(o.is_err(), "a list of values, not one");
        let o = parse_options(Some(&json!(r#"{"Size": ["S", " M ", "s"], "Colour": ["Red"]}"#)), "options").unwrap();
        assert_eq!(o["Size"], vec!["S", "M"], "tidied, a value once whatever its case");
        assert!(parse_options(Some(&json!({"": ["x"]})), "options").is_err() && parse_options(Some(&json!({"Size": []})), "options").is_err());
        assert!(parse_options(None, "options").unwrap().is_empty());
    }

    #[test]
    fn codes_for_values_do_not_repeat() {
        assert_eq!(abbreviation("Large", &[]), "LAR");
        assert_eq!(abbreviation("Large", &["LAR".into()]), "LAR2");
        assert_eq!(abbreviation("Extra Large", &["EXT".into(), "ext2".into()]), "EXT3");
        assert_eq!(abbreviation("--", &[]), "V");
    }

    #[test]
    fn a_disabled_value_disables_the_combinations_that_have_it() {
        let off = vec![("Size".to_string(), "XL".to_string())];
        assert!(has_disabled(&[("size".into(), "xl".into()), ("Colour".into(), "Red".into())], &off));
        assert!(!has_disabled(&[("Size".into(), "L".into())], &off));
    }
}
