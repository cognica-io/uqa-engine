//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Prepare native index generations before the corresponding table or column rename writes.

use super::registry::{IndexPublicationKind, IndexRegistryPublication};
use crate::catalog::index::{index_definition, index_references_column, registry::CatalogIndexes};
use uqa_sql::ast::IndexKey;
use uqa_storage::{CatalogIndexRow, StorageBackendError, StorageBackendResult};

pub fn table_rename(
    rows: &CatalogIndexes,
    from: &str,
    to: &str,
) -> StorageBackendResult<CatalogIndexes> {
    let mut candidate = rows.clone();
    for row in rows.values().filter(|row| row.table_name == from) {
        let mut renamed = row.clone();
        to.clone_into(&mut renamed.table_name);
        candidate.replace(renamed)?;
    }
    Ok(candidate)
}

pub struct PreparedIndexColumnRename {
    rows: CatalogIndexes,
    updates: Vec<CatalogIndexRow>,
}

pub fn column_rename(
    rows: &CatalogIndexes,
    table: &str,
    from: &str,
    to: &str,
) -> StorageBackendResult<PreparedIndexColumnRename> {
    let mut candidate = rows.clone();
    let mut updates = Vec::new();
    for row in rows.values() {
        if row.table_name != table || !index_references_column(row, from)? {
            continue;
        }
        let mut keys: Vec<IndexKey> = serde_json::from_str(&row.columns_json)?;
        let mut definition = index_definition(row)?;
        uqa_sql::catalog::index::stored::rename_column_references(
            &mut keys,
            &mut definition,
            from,
            to,
        )
        .map_err(StorageBackendError::Other)?;
        let mut renamed = row.clone();
        renamed.columns_json = serde_json::to_string(&keys)?;
        renamed.definition_json = Some(serde_json::to_string(&definition)?);
        candidate.replace(renamed.clone())?;
        updates.push(renamed);
    }
    Ok(PreparedIndexColumnRename {
        rows: candidate,
        updates,
    })
}

impl PreparedIndexColumnRename {
    pub fn publish(self, publication: &dyn IndexRegistryPublication) -> StorageBackendResult<()> {
        if self.updates.is_empty() {
            return Ok(());
        }
        for row in self.updates {
            publication.persist_index(&row, IndexPublicationKind::Names)?;
        }
        publication.publish_indexes(self.rows);
        Ok(())
    }
}
