//! A link to a company's master (a warehouse, an account, a cost centre ...) on a record that has a
//! company must limit its choices to that company: `link_filters: {company: {from: company}}`
//! (`{from: parent.company}` in a child table). Without it a form offers every company's records
//! by bare name (four "Stores" in a workspace of four companies) and the save is refused afterwards.
//! This walks the metadata of Commerce's packages (against the masters ERP and CRM define), so a new field
//! cannot forget it. It needs no database.

use ferrum_meta::types::{EntityKind, FieldDef as Field, FieldType, OverlayOp, Package};
use serde_json::json;
use std::collections::BTreeMap;

/// `(entity, field, why the filter cannot apply)`. Empty: nothing in Commerce needs an exemption.
const EXEMPT: &[(&str, &str, &str)] = &[];

/// Every entity's kind and fields, with the fields other packages add to it.
fn entities(pkgs: &[Package]) -> BTreeMap<String, (EntityKind, Vec<Field>)> {
    let mut m: BTreeMap<String, (EntityKind, Vec<Field>)> = BTreeMap::new();
    for p in pkgs {
        for e in &p.entities {
            let x = m.entry(e.name.clone()).or_insert((e.kind, vec![]));
            x.0 = e.kind;
            x.1.extend(e.fields.iter().cloned());
        }
    }
    for p in pkgs {
        for ov in &p.manifest.overlays {
            for op in &ov.ops {
                match op {
                    OverlayOp::AddField { field, .. } => m.entry(ov.entity.clone()).or_insert((EntityKind::Master, vec![])).1.push(field.clone()),
                    OverlayOp::DefineEntity { entity } => {
                        let x = m.entry(entity.name.clone()).or_insert((entity.kind, vec![]));
                        x.1.extend(entity.fields.iter().cloned());
                    }
                    _ => {}
                }
            }
        }
    }
    m
}

fn has_company(fields: &[Field]) -> bool {
    fields.iter().any(|f| f.name == "company" && f.ftype == FieldType::Link && f.to.as_deref() == Some("Company"))
}

/// What is wrong with the links in `pkgs`, given the masters that belong to a company (`masters`, which may
/// come from packages other than `pkgs`, such as ERP's for an app built on it).
pub fn faults(pkgs: &[Package], others: &[Package], exempt: &[(&str, &str, &str)]) -> Vec<String> {
    let mut all: Vec<Package> = others.to_vec();
    all.extend(pkgs.iter().cloned());
    let ents = entities(&all);
    // A master whose company may be empty ("Empty: every company": a numbering series, a promotion) is
    // for every company's records at once, and a filter by equality would refuse it.
    let masters: Vec<&String> = ents
        .iter()
        .filter(|(_, (k, f))| *k == EntityKind::Master && has_company(f) && f.iter().any(|f| f.name == "company" && (f.required || f.read_only)))
        .map(|(n, _)| n)
        .collect();
    let mine = entities(pkgs);
    // Who holds each child entity as a table.
    let mut parents: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for (n, (k, fs)) in &ents {
        if *k != EntityKind::Child {
            for f in fs.iter().filter(|f| f.ftype == FieldType::Table) {
                parents.entry(f.of.as_deref().unwrap_or("")).or_default().push(n);
            }
        }
    }
    let mut bad = Vec::new();
    for (n, (_, fields)) in &mine {
        // Whether the record has a company is known from every package's fields, not only these.
        let (kind, whole) = (&ents[n].0, &ents[n].1);
        let source = if *kind != EntityKind::Child {
            has_company(whole).then_some("company")
        } else if has_company(whole) {
            Some("company")
        } else {
            let ps = parents.get(n.as_str()).cloned().unwrap_or_default();
            (!ps.is_empty() && ps.iter().all(|p| has_company(&ents[*p].1))).then_some("parent.company")
        };
        let Some(source) = source else { continue };
        for f in fields.iter().filter(|f| f.ftype == FieldType::Link && f.name != "company") {
            let Some(to) = f.to.as_deref() else { continue };
            if !masters.iter().any(|m| m.as_str() == to) || exempt.iter().any(|(e, fl, why)| *e == n && *fl == f.name && !why.is_empty()) {
                continue;
            }
            let want = json!({"from": source});
            if f.link_filters.as_ref().and_then(|l| l.get("company")) != Some(&want) {
                bad.push(format!("{n}.{} (to {to}) needs link_filters: {{company: {{from: {source}}}}}", f.name));
            }
        }
    }
    bad.sort();
    bad.dedup();
    bad
}

fn others() -> Vec<Package> {
    let mut p = ferrum_app_erp::packages().expect("ERP's packages");
    p.extend(ferrum_app_crm::packages().expect("CRM's packages"));
    p
}

#[test]
fn every_link_to_a_company_master_is_limited_to_the_records_company() {
    let mine = ferrum_app_commerce::packages().expect("Commerce's packages");
    assert!(mine.iter().any(|p| p.entities.iter().any(|e| e.name == "StoreSettings")));
    let bad = faults(&mine, &others(), EXEMPT);
    assert!(bad.is_empty(), "{} links offer every company's records:\n{}", bad.len(), bad.join("\n"));
}

#[test]
fn the_walk_sees_what_it_should() {
    // A field without the filter is found: the walk is not vacuous.
    let mut mine = ferrum_app_commerce::packages().unwrap();
    let e = mine.iter_mut().flat_map(|p| p.entities.iter_mut()).find(|e| e.name == "StoreSettings").unwrap();
    e.fields.iter_mut().find(|f| f.name == "fulfilment_warehouse").unwrap().link_filters = None;
    let e = mine.iter_mut().flat_map(|p| p.entities.iter_mut()).find(|e| e.name == "StoreWarehouse").unwrap();
    e.fields.iter_mut().find(|f| f.name == "warehouse").unwrap().link_filters = None;
    assert_eq!(
        faults(&mine, &others(), EXEMPT),
        vec![
            "StoreSettings.fulfilment_warehouse (to Warehouse) needs link_filters: {company: {from: company}}",
            "StoreWarehouse.warehouse (to Warehouse) needs link_filters: {company: {from: parent.company}}"
        ]
    );
}
