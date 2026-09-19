//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Existing document payloads migrate before ordinary document access.

use super::KeyValueDocumentStore;
use crate::key_value::codec::{
    decode_stored_document_value_for_migration, decode_value, document_value_is_current,
    encode_stored_document_value, key_with_tag, other_error, read_str, single_str_key,
    string_value,
};
use crate::key_value::{KeyValueStore, TAG_DOCUMENT, TAG_METADATA, TAG_TABLE};
use crate::{StorageBackendResult, TableSchema};

const DOCUMENT_FORMAT_METADATA_KEY: &str = "document_storage_format";
const DOCUMENT_FORMAT_NAME: &str = "record-v2";
const MIGRATION_PAGE_SIZE: usize = 512;

impl KeyValueDocumentStore {
    pub(crate) fn migrate_legacy_storage(store: &dyn KeyValueStore) -> StorageBackendResult<()> {
        let marker = single_str_key(TAG_METADATA, DOCUMENT_FORMAT_METADATA_KEY)?;
        if let Some(format) = store.get(&marker)? {
            if format == DOCUMENT_FORMAT_NAME.as_bytes() {
                return Ok(());
            }
            return Err(other_error(format!(
                "unsupported KeyValue document format `{}`",
                String::from_utf8_lossy(&format)
            )));
        }
        if store.in_transaction() {
            return Err(other_error(
                "cannot migrate KeyValue documents inside an active transaction",
            ));
        }
        store.begin_transaction()?;
        let migration = Self::migrate_legacy_storage_in_transaction(store, &marker);
        match migration {
            Ok(()) => store.commit_transaction(),
            Err(error) => match store.rollback_transaction() {
                Ok(()) => Err(error),
                Err(rollback) => Err(other_error(format!(
                    "{error}; KeyValue document migration rollback also failed: {rollback}"
                ))),
            },
        }
    }

    fn migrate_legacy_storage_in_transaction(
        store: &dyn KeyValueStore,
        marker: &[u8],
    ) -> StorageBackendResult<()> {
        let (known_tables, declared_xmin_tables) = catalog_xmin_tables(store)?;
        let prefix = key_with_tag(TAG_DOCUMENT);
        let mut after = None::<Vec<u8>>;
        loop {
            let page = store.scan_prefix_after(&prefix, after.as_deref(), MIGRATION_PAGE_SIZE)?;
            if page.is_empty() {
                break;
            }
            for (key, value) in page {
                after = Some(key.clone());
                if document_value_is_current(&value) {
                    continue;
                }
                let mut offset = 1;
                let table = read_str(&key, &mut offset)?;
                let preserve_public_xmin =
                    !known_tables.contains(&table) || declared_xmin_tables.contains(&table);
                let document =
                    decode_stored_document_value_for_migration(&value, preserve_public_xmin)?;
                store.put(&key, &encode_stored_document_value(&document)?)?;
            }
        }
        store.put(marker, &string_value(DOCUMENT_FORMAT_NAME))
    }
}

fn catalog_xmin_tables(
    store: &dyn KeyValueStore,
) -> StorageBackendResult<(
    std::collections::BTreeSet<String>,
    std::collections::BTreeSet<String>,
)> {
    let mut known = std::collections::BTreeSet::new();
    let mut declared_xmin = std::collections::BTreeSet::new();
    for (_, value) in store.scan_prefix(&key_with_tag(TAG_TABLE))? {
        let schema = decode_value::<TableSchema>(&value)?;
        let definitions = serde_json::from_str::<Vec<serde_json::Value>>(&schema.columns_json)?;
        let has_declared_xmin = definitions.iter().any(|definition| {
            definition
                .as_object()
                .and_then(|definition| definition.get("name"))
                .and_then(serde_json::Value::as_str)
                == Some("xmin")
        });
        let aliases = schema.relation.canonical_and_legacy_public_names();
        known.extend(aliases.iter().cloned());
        if has_declared_xmin {
            declared_xmin.extend(aliases);
        }
    }
    Ok((known, declared_xmin))
}
