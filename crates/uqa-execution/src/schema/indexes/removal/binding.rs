//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Rebind explicit index names and authority after waiting for the indexed table.

use super::{ddl_storage_error, IndexRemovalCatalog, IndexRemovalPrivileges};
use crate::row_locks::{
    binding::{bind_relation, RelationBinding, RelationLockSession},
    RelationLockMode,
};
use std::collections::BTreeSet;
use uqa_sql::{ast::DropStmt, schema::indexes::removal::resolve_drop_index_name, SQLError};
use uqa_storage::CatalogIndexRow;

pub(super) fn bind_drop_targets(
    catalog: &dyn IndexRemovalCatalog,
    privileges: &dyn IndexRemovalPrivileges,
    session: &dyn RelationLockSession,
    statement: &DropStmt,
    notice: &mut dyn FnMut(&str),
) -> Result<Vec<CatalogIndexRow>, SQLError> {
    let mut indexes = Vec::new();
    let mut seen = BTreeSet::new();
    for requested in &statement.names {
        let bound = bind_relation(
            session,
            RelationLockMode::AccessExclusive,
            false,
            || {
                let Some(canonical) = resolve_drop_index_name(
                    catalog.resolve_relation_kind(requested)?,
                    requested,
                    statement.if_exists,
                    notice,
                )?
                else {
                    return Ok(None);
                };
                let row = catalog
                    .bound_catalog_index(&canonical)
                    .map_err(|error| ddl_storage_error("DROP INDEX", error))?
                    .ok_or_else(|| {
                        SQLError::Internal(format!(
                            "resolved index `{canonical}` has no bound catalog row"
                        ))
                    })?;
                let definition = crate::catalog::index::index_definition(&row)
                    .map_err(|error| ddl_storage_error("DROP INDEX", error))?;
                Ok(Some(RelationBinding {
                    name: row.table_name.clone(),
                    object_id: definition.catalog.map(|catalog| catalog.identity.object_id),
                    value: row,
                }))
            },
            |binding| {
                let row = &binding.value;
                privileges.ensure_drop_authority(row)?;
                if binding.object_id.is_none() && !catalog.has_constraint_index(&row.relation) {
                    return Err(SQLError::Internal(format!(
                        "index `{}` has no catalog identity",
                        row.relation.qualified_name()
                    )));
                }
                Ok(())
            },
        )?;
        if let Some(bound) = bound {
            if seen.insert(bound.value.relation.clone()) {
                indexes.push(bound.value);
            }
        }
    }
    Ok(indexes)
}

#[cfg(test)]
mod tests;
