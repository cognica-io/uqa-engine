//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Whether each table's single integer primary key names its rows' identities, and the one-time check of the tables an earlier version wrote.

use std::sync::atomic::Ordering;

use super::{CatalogFacade, Engine, PersistentStorageBackend, StorageBackendResult};
use uqa_sql::semantics::key_identity::{is_key_document_id, key_document_id};
use uqa_storage::ValueIndexKey;

/// Present once every table of the database has been checked, by the open that first ran this version, or because no earlier version wrote it.
const VERIFIED_KEY: &str = "uqa.integer_key_identities.verified.v1";

/// Present for a table that an earlier version left with a row at an identity its key does not name.
fn indexed_key(object_id: [u8; 16]) -> String {
    use std::fmt::Write;
    let mut key = String::from("uqa.integer_key_identities.indexed.v1:");
    for byte in object_id {
        write!(key, "{byte:02x}").expect("writing to a string cannot fail");
    }
    key
}

/// The single integer primary-key column of a table.
fn single_integer_key(columns: &[uqa_sql::ast::ColumnDef]) -> Option<&str> {
    let mut keys = columns.iter().filter(|column| column.primary_key);
    let key = keys.next()?;
    (keys.next().is_none() && key.ty.is_integer()).then_some(key.name.as_str())
}

impl Engine {
    /// Whether a table restored from `catalog` maps its integer key to identities. Until the database has been checked, which an open that may migrate does first, a table with such a key resolves it through the key's index.
    pub(super) fn restored_key_mapping(
        catalog: &dyn CatalogFacade,
        columns: &[uqa_sql::ast::ColumnDef],
        object_id: [u8; 16],
    ) -> StorageBackendResult<bool> {
        if single_integer_key(columns).is_none() {
            return Ok(true);
        }
        if catalog.get_metadata(VERIFIED_KEY)?.is_none() {
            return Ok(false);
        }
        Ok(catalog
            .get_metadata(&indexed_key(object_id))?
            .is_none_or(|value| value.is_empty()))
    }

    /// Check, once for a database an earlier version wrote, that each table with a single integer primary key holds every row whose key lies below `KEY_IDENTITY_LIMIT` at the identity equal to its key and no other row below the limit. Earlier versions stored a negative key, or a key next to a sequence column that chose identities, at another identity. A table that holds such a row is recorded and resolves its keys through the key's index from then on; the others map their keys.
    pub(super) fn verify_integer_key_identities(
        &self,
        catalog: &dyn CatalogFacade,
        backend: &dyn PersistentStorageBackend,
    ) -> StorageBackendResult<()> {
        if catalog.get_metadata(VERIFIED_KEY)?.is_some() {
            return Ok(());
        }
        for (relation, table) in self.storage.tables.read().iter() {
            let columns = table.columns.snapshot();
            let Some(key) = single_integer_key(&columns) else {
                continue;
            };
            let name = relation.qualified_name();
            let field = ValueIndexKey::Column(key.into());
            let rows = if let Some(rows) = backend.load_btree_index(&name, &field)? {
                rows
            } else {
                let documents = table.document_store.read();
                let mut rows = Vec::new();
                for doc_id in documents.doc_ids()? {
                    let value = documents
                        .get_field(doc_id, key)?
                        .unwrap_or(uqa_core::Value::Null);
                    rows.push((doc_id, value));
                }
                rows
            };
            let mapped = rows.iter().all(|(doc_id, value)| {
                !is_key_document_id(*doc_id) || key_document_id(value) == Some(*doc_id)
            });
            if !mapped {
                catalog.set_metadata(&indexed_key(table.object_id()), "true")?;
            }
            table.maps_integer_keys.store(mapped, Ordering::Release);
        }
        catalog.set_metadata(VERIFIED_KEY, "true")
    }
}
