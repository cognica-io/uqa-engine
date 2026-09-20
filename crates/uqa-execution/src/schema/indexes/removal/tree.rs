//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Capture the complete index dependency tree before any physical deletion.

use super::{ddl_storage_error, CatalogIndexRow, IndexRemovalContext, SQLError};
use std::collections::BTreeMap;
use uqa_core::RelationIdentity;

pub(super) fn bind_removals(
    context: &IndexRemovalContext<'_>,
    indexes: &[CatalogIndexRow],
    cascade: bool,
) -> Result<BTreeMap<RelationIdentity, CatalogIndexRow>, SQLError> {
    let rows = context
        .catalog
        .list_catalog_indexes()
        .map_err(|error| ddl_storage_error("DROP INDEX registry", error))?
        .into_iter()
        .map(|row| (row.relation.clone(), row))
        .collect();
    let mut tree = std::collections::BTreeMap::new();
    for root in indexes {
        for row in super::super::registry::lifecycle::descendants(&rows, &root.relation)
            .map_err(|error| ddl_storage_error("DROP INDEX ancestry", error))?
        {
            if row.relation != root.relation
                && crate::catalog::index::index_definition(&row)
                    .map_err(|error| ddl_storage_error("DROP INDEX owner", error))?
                    .relationships
                    .owning_constraint
                    .is_some()
                && !cascade
            {
                return Err(SQLError::Routine {
                    sqlstate: "2BP01".into(),
                    message: format!(
                        "cannot drop index {} because constraint {} on table {} depends on it",
                        root.relation.qualified_name(),
                        row.relation.name,
                        row.table_name
                    ),
                });
            }
            tree.insert(row.relation.clone(), row);
        }
    }
    Ok(tree)
}
