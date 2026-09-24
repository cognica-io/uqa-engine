//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retain an index and its heap together only after both definition locks have been revalidated.

use crate::catalog::index::index_definition;
use crate::row_locks::{
    binding::{acquire_relation, RelationLockSession},
    RelationLockMode,
};
use uqa_sql::{catalog::errors::storage_error, SQLError};
use uqa_storage::CatalogIndexRow;

pub(super) fn bind_index_and_table(
    session: &dyn RelationLockSession,
    mut resolve: impl FnMut() -> Result<Option<CatalogIndexRow>, SQLError>,
    mut validate: impl FnMut(&CatalogIndexRow) -> Result<(), SQLError>,
) -> Result<Option<CatalogIndexRow>, SQLError> {
    loop {
        let Some(initial) = resolve()? else {
            return Ok(None);
        };
        validate(&initial)?;
        let heap = acquire_relation(
            session,
            &initial.table_name,
            RelationLockMode::AccessExclusive,
            false,
        )?;
        session.refresh_after_wait()?;
        let Some(current) = resolve()? else {
            return Ok(None);
        };
        if !same_index(&initial, &current)? {
            continue;
        }
        validate(&current)?;
        let index = acquire_relation(
            session,
            &current.relation.qualified_name(),
            RelationLockMode::AccessExclusive,
            false,
        )?;
        session.refresh_after_wait()?;
        let Some(current) = resolve()? else {
            return Ok(None);
        };
        if !same_index(&initial, &current)? {
            continue;
        }
        validate(&current)?;
        heap.retain();
        index.retain();
        return Ok(Some(current));
    }
}

fn same_index(left: &CatalogIndexRow, right: &CatalogIndexRow) -> Result<bool, SQLError> {
    let identity = |row| {
        index_definition(row)
            .map_err(|error| storage_error("index definition binding", &error))?
            .catalog
            .map(|catalog| (catalog.identity.object_id, catalog.table_object_id))
            .ok_or_else(|| SQLError::Internal("index has no catalog identity".into()))
    };
    Ok(left.relation == right.relation
        && left.table_name == right.table_name
        && identity(left)? == identity(right)?)
}
