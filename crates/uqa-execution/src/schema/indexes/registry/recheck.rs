//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Recheck every prepared table and index after address reservations may have waited.

use super::{
    difference, same_row, validation, BTreeMap, BTreeSet, CatalogIndexRow, IndexRegistryContext,
    RelationIdentity, StorageBackendError, StorageBackendResult,
};
use crate::catalog::{CatalogReadView, CatalogTableSnapshot};

pub(super) fn validate(
    context: &IndexRegistryContext<'_>,
    before: &CatalogReadView,
    candidate: &CatalogReadView,
    rows: &BTreeMap<RelationIdentity, CatalogIndexRow>,
    root: &RelationIdentity,
) -> StorageBackendResult<()> {
    let current = context.identities.catalog.current_catalog_snapshot();
    let change = difference(&before.snapshot().definitions.catalog_indexes, rows)?;
    let mut affected = BTreeSet::from([root.clone()]);
    loop {
        let children = before
            .snapshot()
            .tables
            .iter()
            .filter(|(_, table)| {
                table.hierarchy.is_partition()
                    && table.hierarchy.parents.first().is_some_and(|parent| {
                        affected.iter().any(|name| name.qualified_name() == *parent)
                    })
            })
            .map(|(name, _)| name.clone())
            .collect::<Vec<_>>();
        let previous = affected.len();
        affected.extend(children);
        if previous == affected.len() {
            break;
        }
    }
    for row in change.upserts.iter().chain(&change.removals) {
        affected
            .insert(RelationIdentity::from_legacy_name(&row.table_name).map_err(super::invalid)?);
        let old = before
            .snapshot()
            .definitions
            .catalog_indexes
            .get(&row.relation);
        let now = current
            .snapshot()
            .definitions
            .catalog_indexes
            .get(&row.relation);
        if !match (old, now) {
            (None, None) => true,
            (Some(old), Some(now)) => same_row(old, now),
            _ => false,
        } {
            return Err(changed(root));
        }
    }
    let mut snapshot = current.snapshot().clone();
    for name in &affected {
        let old = before
            .snapshot()
            .tables
            .get(name)
            .ok_or_else(|| changed(name))?;
        let now = current
            .snapshot()
            .tables
            .get(name)
            .ok_or_else(|| changed(name))?;
        if old.object_id != now.object_id || declaration(old)? != declaration(now)? {
            return Err(changed(name));
        }
        let selected = |catalog: &CatalogReadView| {
            catalog
                .snapshot()
                .definitions
                .catalog_indexes
                .values()
                .filter(|row| row.table_name == name.qualified_name())
                .cloned()
                .collect::<Vec<_>>()
        };
        let old_indexes = selected(before);
        let now_indexes = selected(&current);
        if old_indexes.len() != now_indexes.len()
            || old_indexes
                .iter()
                .zip(&now_indexes)
                .any(|(old, now)| !same_row(old, now))
        {
            return Err(changed(name));
        }
        snapshot
            .tables
            .insert(name.clone(), candidate.snapshot().tables[name].clone());
    }
    let mut rebased = current
        .snapshot()
        .definitions
        .catalog_indexes
        .as_ref()
        .clone();
    for row in change.removals {
        rebased.remove(&row.relation);
    }
    for row in change.upserts {
        rebased.insert(row.relation.clone(), row);
    }
    validation::validate(&CatalogReadView::new(snapshot), &rebased)
}

fn declaration(table: &CatalogTableSnapshot) -> StorageBackendResult<Vec<u8>> {
    Ok(serde_json::to_vec(&(
        &*table.columns,
        &*table.checks,
        &*table.foreign_keys,
        &*table.keys,
        &*table.hierarchy,
    ))?)
}

fn changed(name: &RelationIdentity) -> StorageBackendError {
    StorageBackendError::backend(
        "index registry",
        uqa_sql::SQLError::Routine {
            sqlstate: "40001".into(),
            message: format!(
                "index owner `{}` changed during address reservation",
                name.qualified_name()
            ),
        },
    )
}
