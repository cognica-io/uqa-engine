//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The enforced keys of the tables a session writes, kept while the definitions they were read from are the same objects.

use std::{collections::BTreeMap, sync::Arc};

use parking_lot::Mutex;
use uqa_core::RelationIdentity;
use uqa_sql::{ast::TableKeyConstraint, catalog::index::EnforcedKey};
use uqa_storage::{CatalogIndexRow, StorageBackendResult};

type IndexRows = BTreeMap<RelationIdentity, CatalogIndexRow>;

/// Tables whose keys are kept. A statement writes its target and the tables its referential actions reach.
const TABLES: usize = 8;

struct Entry {
    table: String,
    constraints: Vec<TableKeyConstraint>,
    indexes: Arc<IndexRows>,
    keys: Vec<EnforcedKey>,
}

/// Every written row asks for its table's enforced keys twice, and finding them decodes each index definition of the catalog. The keys of a table depend on its declared constraints and on the index rows, so they are kept for the same constraints and the same index rows. Arc identity follows copy-on-write catalog publication, refresh and rollback; no independent invalidation counter can omit a mutation path. The keys of a partition's index also name its ancestors, which other tables' definitions decide, and are not kept.
#[derive(Default)]
pub struct EnforcedKeyCache {
    entries: Mutex<Vec<Entry>>,
}

impl EnforcedKeyCache {
    pub fn keys(
        &self,
        table: &str,
        constraints: Vec<TableKeyConstraint>,
        indexes: &Arc<IndexRows>,
        find: impl FnOnce(Vec<TableKeyConstraint>) -> StorageBackendResult<Vec<EnforcedKey>>,
    ) -> StorageBackendResult<Vec<EnforcedKey>> {
        if let Some(entry) = self.entries.lock().iter().find(|entry| {
            entry.table == table
                && Arc::ptr_eq(&entry.indexes, indexes)
                && entry.constraints == constraints
        }) {
            return Ok(entry.keys.clone());
        }
        let keys = find(constraints.clone())?;
        if keys.iter().all(|key| key.index_ancestors.is_empty()) {
            let mut entries = self.entries.lock();
            entries.retain(|entry| entry.table != table);
            if entries.len() == TABLES {
                entries.remove(0);
            }
            entries.push(Entry {
                table: table.to_owned(),
                constraints,
                indexes: Arc::clone(indexes),
                keys: keys.clone(),
            });
        }
        Ok(keys)
    }
}

#[cfg(test)]
mod tests;
