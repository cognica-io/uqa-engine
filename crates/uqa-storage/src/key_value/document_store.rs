//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Document-store adapter over an ordered key/value store.

use super::codec::{
    decode_document_value, decode_stored_document_value,
    decode_stored_document_value_for_migration, decode_value, document_key, document_key_prefix,
    document_value_is_current, encode_stored_document_value, key_with_tag, other_error, read_str,
    read_u64, single_str_key, string_value,
};
use super::{
    Arc, DocId, Document, DocumentMetadata, DocumentStore, KeyValueStore, StorageBackendResult,
    StoredDocument, Value, TAG_DOCUMENT, TAG_METADATA, TAG_TABLE,
};
use crate::TableSchema;

const DOCUMENT_FORMAT_METADATA_KEY: &str = "document_storage_format";
const DOCUMENT_FORMAT_NAME: &str = "record-v2";
const MIGRATION_PAGE_SIZE: usize = 512;

/// Document store implemented over [`KeyValueStore`].
#[derive(Clone)]
pub struct KeyValueDocumentStore {
    store: Arc<dyn KeyValueStore>,
    table: String,
}

impl KeyValueDocumentStore {
    pub fn new(store: Arc<dyn KeyValueStore>, table: impl Into<String>) -> Self {
        Self {
            store,
            table: table.into(),
        }
    }

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

impl DocumentStore for KeyValueDocumentStore {
    fn put(&mut self, doc_id: DocId, document: Document) -> StorageBackendResult<()> {
        let metadata = self.get_metadata(doc_id)?.unwrap_or_default();
        self.put_stored(doc_id, StoredDocument::with_metadata(document, metadata))
    }

    fn get(&self, doc_id: DocId) -> StorageBackendResult<Option<Document>> {
        self.store
            .get(&document_key(&self.table, doc_id)?)?
            .map(|bytes| decode_document_value(&bytes))
            .transpose()
    }

    fn put_stored(&mut self, doc_id: DocId, document: StoredDocument) -> StorageBackendResult<()> {
        let (fields, metadata) = document.into_parts();
        let fields = fields
            .into_iter()
            .filter(|(_, value)| !matches!(value, Value::Null))
            .collect();
        let document = StoredDocument::with_metadata(fields, metadata);
        let value = encode_stored_document_value(&document)?;
        self.store.put(&document_key(&self.table, doc_id)?, &value)
    }

    fn get_stored(&self, doc_id: DocId) -> StorageBackendResult<Option<StoredDocument>> {
        self.store
            .get(&document_key(&self.table, doc_id)?)?
            .map(|bytes| decode_stored_document_value(&bytes))
            .transpose()
    }

    fn get_metadata(&self, doc_id: DocId) -> StorageBackendResult<Option<DocumentMetadata>> {
        self.get_stored(doc_id)
            .map(|document| document.map(|document| document.metadata()))
    }

    fn contains_doc_id(&self, doc_id: DocId) -> StorageBackendResult<bool> {
        self.store.contains_key(&document_key(&self.table, doc_id)?)
    }

    fn delete(&mut self, doc_id: DocId) -> StorageBackendResult<()> {
        self.store.delete(&document_key(&self.table, doc_id)?)
    }

    fn clear(&mut self) -> StorageBackendResult<()> {
        self.store
            .delete_prefix(&document_key_prefix(&self.table)?)
            .map(|_| ())
    }

    fn doc_ids(&self) -> StorageBackendResult<Vec<DocId>> {
        let mut out = Vec::new();
        for (key, _) in self.store.scan_prefix(&document_key_prefix(&self.table)?)? {
            let mut offset = 1;
            let _table = read_str(&key, &mut offset)?;
            out.push(read_u64(&key, &mut offset)?);
        }
        Ok(out)
    }

    fn next_doc_id(&self, after: Option<DocId>) -> StorageBackendResult<Option<DocId>> {
        Ok(self.next_doc_ids(after, 1)?.into_iter().next())
    }

    fn next_doc_ids(&self, after: Option<DocId>, limit: usize) -> StorageBackendResult<Vec<DocId>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let prefix = document_key_prefix(&self.table)?;
        let after_key = after
            .map(|doc_id| document_key(&self.table, doc_id))
            .transpose()?;
        let mut out = Vec::with_capacity(limit);
        for key in self
            .store
            .scan_prefix_keys_after(&prefix, after_key.as_deref(), limit)?
        {
            let mut offset = 1;
            let _table = read_str(&key, &mut offset)?;
            let doc_id = read_u64(&key, &mut offset)?;
            out.push(doc_id);
        }
        Ok(out)
    }

    fn len(&self) -> StorageBackendResult<usize> {
        Ok(self
            .store
            .scan_prefix(&document_key_prefix(&self.table)?)?
            .len())
    }

    fn snapshot(&self) -> StorageBackendResult<Arc<dyn DocumentStore>> {
        Ok(Arc::new(self.clone()))
    }
}
