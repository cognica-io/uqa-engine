//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Immutable trigger collections and borrowed rule definitions selected by catalog OID.

use std::{collections::BTreeMap, sync::OnceLock};

use super::{
    CatalogReadView, RelationIdentity, RelationNameResolution, SQLError, StoredRule, StoredTrigger,
};
use crate::catalog::{
    cache::{CatalogDerivation, OrderedOidLookup},
    view::StoredView,
};

#[derive(Default)]
pub(in crate::catalog) struct EventDefinitions {
    triggers: CatalogDerivation<Triggers>,
    rules: OnceLock<BTreeMap<i64, (RelationIdentity, String)>>,
    views: OnceLock<ViewDefinitions>,
}

struct Triggers {
    rows: Vec<(StoredTrigger, i64)>,
    by_oid: OnceLock<OrderedOidLookup<usize>>,
}

struct ViewDefinitions {
    names: Vec<RelationIdentity>,
    by_rule: BTreeMap<i64, usize>,
    by_relation: BTreeMap<i64, usize>,
    missing_identity: bool,
}

fn retained_triggers<'a>(
    catalog: &'a CatalogReadView,
    resolution: &RelationNameResolution,
) -> Result<&'a Triggers, SQLError> {
    catalog.event_definitions.triggers.get_or_try_init(|| {
        Ok(Triggers {
            rows: super::build_catalog_triggers(catalog, resolution)?,
            by_oid: OnceLock::new(),
        })
    })
}

pub(crate) fn catalog_triggers<'a>(
    catalog: &'a CatalogReadView,
    resolution: &RelationNameResolution,
) -> Result<&'a [(StoredTrigger, i64)], SQLError> {
    Ok(&retained_triggers(catalog, resolution)?.rows)
}

pub(super) fn trigger_by_oid<'a>(
    catalog: &'a CatalogReadView,
    resolution: &RelationNameResolution,
    oid: i64,
) -> Result<Option<&'a StoredTrigger>, SQLError> {
    let triggers = retained_triggers(catalog, resolution)?;
    let lookup =
        triggers.by_oid.get_or_init(|| {
            let mut bound = resolution.clone();
            bound.set_lookup_mode(crate::catalog::RelationLookupMode::Bound);
            OrderedOidLookup::build(triggers.rows.iter().enumerate().map(
                |(position, (trigger, _))| {
                    #[cfg(test)]
                    TRIGGER_ADDRESSES.set(TRIGGER_ADDRESSES.get() + 1);
                    super::trigger_catalog_oid(catalog, &bound, trigger).map(|oid| (oid, position))
                },
            ))
        });
    Ok(lookup.get(oid)?.map(|&position| &triggers.rows[position].0))
}

pub(super) fn rule_by_oid(catalog: &CatalogReadView, oid: i64) -> Option<&StoredRule> {
    let lookup = catalog.event_definitions.rules.get_or_init(|| {
        let mut by_oid = BTreeMap::new();
        for (relation, rules) in catalog.snapshot().definitions.rules.iter() {
            for (name, rule) in rules {
                #[cfg(test)]
                RULE_ADDRESSES.set(RULE_ADDRESSES.get() + 1);
                by_oid
                    .entry(super::rule_catalog_oid(rule))
                    .or_insert_with(|| (relation.clone(), name.clone()));
            }
        }
        by_oid
    });
    lookup
        .get(&oid)
        .map(|(relation, name)| &catalog.snapshot().definitions.rules[relation][name])
}

fn retained_views(catalog: &CatalogReadView) -> &ViewDefinitions {
    catalog.event_definitions.views.get_or_init(|| {
        let mut names = Vec::new();
        let mut by_rule = BTreeMap::new();
        let mut by_relation = BTreeMap::new();
        let mut missing_identity = false;
        for (name, view) in super::catalog_view_rules(catalog) {
            #[cfg(test)]
            VIEW_ADDRESSES.set(VIEW_ADDRESSES.get() + 1);
            let position = names.len();
            names.push(name.clone());
            by_relation
                .entry(super::super::view_relation_oid(view))
                .or_insert(position);
            if !missing_identity {
                match view.relation_oids().rule {
                    Some(oid) => {
                        by_rule.entry(i64::from(oid)).or_insert(position);
                    }
                    None => missing_identity = true,
                }
            }
        }
        ViewDefinitions {
            names,
            by_rule,
            by_relation,
            missing_identity,
        }
    })
}

pub(in crate::catalog::projection) fn view_by_oid(
    catalog: &CatalogReadView,
    oid: i64,
) -> Option<&StoredView> {
    let lookup = retained_views(catalog);
    lookup
        .by_relation
        .get(&oid)
        .map(|&position| &catalog.snapshot().definitions.views[&lookup.names[position]])
}

pub(super) fn view_rule_by_oid(
    catalog: &CatalogReadView,
    oid: i64,
) -> Option<(&RelationIdentity, &StoredView)> {
    let lookup = retained_views(catalog);
    if let Some(&position) = lookup.by_rule.get(&oid) {
        let name = &lookup.names[position];
        return Some((name, &catalog.snapshot().definitions.views[name]));
    }
    // The original ordered search reaches this invariant only if no earlier view matched.
    assert!(
        !lookup.missing_identity,
        "a view's OIDs include its _RETURN rule"
    );
    None
}

#[cfg(test)]
thread_local! {
    static TRIGGER_COPIES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static TRIGGER_ADDRESSES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static RULE_ADDRESSES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static VIEW_ADDRESSES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(super) fn record_trigger_copy() {
    TRIGGER_COPIES.set(TRIGGER_COPIES.get() + 1);
}

#[cfg(test)]
mod tests;
