//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Borrow validated constraint definitions and select OIDs in their immutable catalog generation.

use std::{collections::BTreeMap, sync::OnceLock};

use super::{CatalogReadView, ConstraintCatalogRow, RelationNameResolution, SQLError};
use crate::catalog::projection::pg_catalog::constraint_row_oid;

pub(in crate::catalog) struct ConstraintDefinitions {
    rows: Vec<ConstraintCatalogRow>,
    by_oid: BTreeMap<i64, usize>,
    domains: OnceLock<BTreeMap<i64, DomainLocation>>,
}

struct DomainLocation {
    name: String,
    check: Option<usize>,
}

pub(crate) enum DomainConstraint<'a> {
    NotNull,
    Check(&'a uqa_sql::ast::DomainCheck),
}

fn retained<'a>(
    catalog: &'a CatalogReadView,
    resolution: &RelationNameResolution,
) -> Result<&'a ConstraintDefinitions, SQLError> {
    catalog.constraint_definitions.get_or_try_init(|| {
        let rows = super::build_constraint_catalog_rows(catalog, resolution)?;
        let mut by_oid = BTreeMap::new();
        for (position, row) in rows.iter().enumerate() {
            // Complete validation precedes selection; duplicate identities retain the first row.
            by_oid.entry(constraint_row_oid(row)).or_insert(position);
        }
        Ok(ConstraintDefinitions {
            rows,
            by_oid,
            domains: OnceLock::new(),
        })
    })
}

pub(crate) fn constraint_catalog_rows<'a>(
    catalog: &'a CatalogReadView,
    resolution: &RelationNameResolution,
) -> Result<&'a [ConstraintCatalogRow], SQLError> {
    Ok(&retained(catalog, resolution)?.rows)
}

pub(crate) fn constraint_catalog_row_by_oid<'a>(
    catalog: &'a CatalogReadView,
    resolution: &RelationNameResolution,
    oid: i64,
) -> Result<Option<&'a ConstraintCatalogRow>, SQLError> {
    let constraints = retained(catalog, resolution)?;
    Ok(constraints
        .by_oid
        .get(&oid)
        .map(|&position| &constraints.rows[position]))
}

pub(crate) fn domain_constraint_by_oid<'a>(
    catalog: &'a CatalogReadView,
    resolution: &RelationNameResolution,
    oid: i64,
) -> Result<Option<DomainConstraint<'a>>, SQLError> {
    // Relation inquiries do not need domain metadata. Build addresses only after they miss.
    let domains = retained(catalog, resolution)?
        .domains
        .get_or_init(|| domain_addresses(catalog));
    Ok(domains.get(&oid).map(|location| {
        let domain = &catalog.snapshot().definitions.domains[&location.name];
        location
            .check
            .map_or(DomainConstraint::NotNull, |position| {
                DomainConstraint::Check(&domain.definition.checks[position])
            })
    }))
}

fn domain_addresses(catalog: &CatalogReadView) -> BTreeMap<i64, DomainLocation> {
    let mut addresses = BTreeMap::new();
    for (name, domain) in catalog.snapshot().definitions.domains.iter() {
        #[cfg(test)]
        DOMAIN_READS.set(DOMAIN_READS.get() + 1);
        let not_null = domain
            .definition
            .not_null
            .iter()
            .filter_map(|constraint| constraint.catalog_identity.map(|id| (id.oid, None)));
        let checks = domain
            .definition
            .checks
            .iter()
            .enumerate()
            .filter_map(|(position, check)| {
                check.catalog_identity.map(|id| (id.oid, Some(position)))
            });
        for (oid, check) in not_null.chain(checks) {
            addresses.entry(oid).or_insert_with(|| DomainLocation {
                name: name.clone(),
                check,
            });
        }
    }
    addresses
}

#[cfg(test)]
thread_local! {
    static TABLE_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static DOMAIN_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(super) fn record_table_read() {
    TABLE_READS.set(TABLE_READS.get() + 1);
}

#[cfg(test)]
mod tests;
