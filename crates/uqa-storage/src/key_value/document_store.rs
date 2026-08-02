//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Document-store adapter over an ordered key/value store.

use super::codec::{
    decode_document_value, document_key, document_key_prefix, encode_document_value, read_str,
    read_u64,
};
use super::{Arc, DocId, Document, DocumentStore, KeyValueStore, StorageBackendResult, Value};

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
}

impl DocumentStore for KeyValueDocumentStore {
    fn put(&mut self, doc_id: DocId, document: Document) -> StorageBackendResult<()> {
        let document: Document = document
            .into_iter()
            .filter(|(_, value)| !matches!(value, Value::Null))
            .collect();
        let value = encode_document_value(&document)?;
        self.store.put(&document_key(&self.table, doc_id)?, &value)
    }

    fn get(&self, doc_id: DocId) -> StorageBackendResult<Option<Document>> {
        self.store
            .get(&document_key(&self.table, doc_id)?)?
            .map(|bytes| decode_document_value(&bytes))
            .transpose()
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
