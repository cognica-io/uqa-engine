//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Validate index metadata once and borrow complete or selected relations from that generation.

use super::{CatalogIndexRelation, CatalogReadView, RelationIdentity, SQLError};
use std::collections::BTreeMap;

pub(in crate::catalog) struct IndexRelations {
    rows: Vec<CatalogIndexRelation>,
    by_oid: BTreeMap<i64, usize>,
    by_name: BTreeMap<RelationIdentity, usize>,
    by_constraint: BTreeMap<[u8; 16], usize>,
}

fn retained(catalog: &CatalogReadView) -> Result<&IndexRelations, SQLError> {
    catalog.index_relations.get_or_try_init(|| {
        let rows = super::build_index_relations(catalog)?;
        let mut by_oid = BTreeMap::new();
        let mut by_name = BTreeMap::new();
        let mut by_constraint = BTreeMap::new();
        for (position, row) in rows.iter().enumerate() {
            // Identity-claim validation reports collisions separately; inquiries preserve the first row's order.
            by_oid.entry(row.oid()).or_insert(position);
            by_name.entry(row.relation.clone()).or_insert(position);
            if let Some(owner) = row.definition.relationships.owning_constraint {
                by_constraint.entry(owner).or_insert(position);
            }
        }
        Ok(IndexRelations {
            rows,
            by_oid,
            by_name,
            by_constraint,
        })
    })
}

pub(super) fn catalog_index_relations(
    catalog: &CatalogReadView,
) -> Result<&[CatalogIndexRelation], SQLError> {
    Ok(&retained(catalog)?.rows)
}

pub(crate) fn catalog_index_by_oid(
    catalog: &CatalogReadView,
    oid: i64,
) -> Result<Option<&CatalogIndexRelation>, SQLError> {
    let indexes = retained(catalog)?;
    Ok(indexes
        .by_oid
        .get(&oid)
        .map(|&position| &indexes.rows[position]))
}

pub(crate) fn catalog_index_for_constraint(
    catalog: &CatalogReadView,
    owner: Option<[u8; 16]>,
) -> Result<Option<&CatalogIndexRelation>, SQLError> {
    let indexes = retained(catalog)?;
    Ok(owner
        .and_then(|owner| indexes.by_constraint.get(&owner))
        .map(|&position| &indexes.rows[position]))
}

pub(crate) fn catalog_index_by_name<'a>(
    catalog: &'a CatalogReadView,
    name: &RelationIdentity,
) -> Result<Option<&'a CatalogIndexRelation>, SQLError> {
    let indexes = retained(catalog)?;
    Ok(indexes
        .by_name
        .get(name)
        .map(|&position| &indexes.rows[position]))
}

#[cfg(test)]
thread_local! {
    static DECODED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(super) fn record_decode() {
    DECODED.set(DECODED.get() + 1);
}

#[cfg(test)]
mod tests;
