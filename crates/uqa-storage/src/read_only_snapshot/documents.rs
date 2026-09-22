//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::collections::BTreeMap;
use std::sync::Arc;

use uqa_core::{DocId, PathSegment, Value};

use crate::document_store::{Document, DocumentMetadata, SharedDocumentRow, StoredDocument};
use crate::{DocumentStore, StorageBackendResult};

use super::{read_only_error, ReadOnlySnapshot};

impl DocumentStore for ReadOnlySnapshot<dyn DocumentStore> {
    fn put_stored(
        &mut self,
        _doc_id: DocId,
        _document: StoredDocument,
    ) -> StorageBackendResult<()> {
        Err(read_only_error())
    }

    fn get_stored(&self, doc_id: DocId) -> StorageBackendResult<Option<StoredDocument>> {
        self.0.get_stored(doc_id)
    }

    fn put(&mut self, _doc_id: DocId, _document: Document) -> StorageBackendResult<()> {
        Err(read_only_error())
    }

    fn get(&self, doc_id: DocId) -> StorageBackendResult<Option<Document>> {
        self.0.get(doc_id)
    }

    fn get_stored_many(
        &self,
        doc_ids: &[DocId],
    ) -> StorageBackendResult<BTreeMap<DocId, StoredDocument>> {
        self.0.get_stored_many(doc_ids)
    }

    fn get_metadata(&self, doc_id: DocId) -> StorageBackendResult<Option<DocumentMetadata>> {
        self.0.get_metadata(doc_id)
    }

    fn contains_doc_id(&self, doc_id: DocId) -> StorageBackendResult<bool> {
        self.0.contains_doc_id(doc_id)
    }

    fn delete(&mut self, _doc_id: DocId) -> StorageBackendResult<()> {
        Err(read_only_error())
    }

    fn clear(&mut self) -> StorageBackendResult<()> {
        Err(read_only_error())
    }

    fn get_field(&self, doc_id: DocId, field: &str) -> StorageBackendResult<Option<Value>> {
        self.0.get_field(doc_id, field)
    }

    fn find_doc_id_by_field(
        &self,
        field: &str,
        value: &Value,
    ) -> StorageBackendResult<Option<DocId>> {
        self.0.find_doc_id_by_field(field, value)
    }

    fn patch_fields(
        &mut self,
        _doc_id: DocId,
        _updates: &BTreeMap<String, Value>,
    ) -> StorageBackendResult<bool> {
        Err(read_only_error())
    }

    fn get_many(&self, doc_ids: &[DocId]) -> StorageBackendResult<BTreeMap<DocId, Document>> {
        self.0.get_many(doc_ids)
    }

    fn get_fields_multi(
        &self,
        doc_ids: &[DocId],
        fields: &[&str],
    ) -> StorageBackendResult<BTreeMap<DocId, Vec<Value>>> {
        self.0.get_fields_multi(doc_ids, fields)
    }

    fn for_each_fields_multi(
        &self,
        doc_ids: &[DocId],
        fields: &[&str],
        visitor: &mut dyn FnMut(DocId, Vec<Value>) -> bool,
    ) -> StorageBackendResult<()> {
        self.0.for_each_fields_multi(doc_ids, fields, visitor)
    }

    fn for_each_fields_multi_ref(
        &self,
        doc_ids: &[DocId],
        fields: &[&str],
        visitor: &mut dyn FnMut(DocId, &[&Value]) -> bool,
    ) -> StorageBackendResult<()> {
        self.0.for_each_fields_multi_ref(doc_ids, fields, visitor)
    }

    fn for_each_fields_multi_ref_with_presence(
        &self,
        doc_ids: &[DocId],
        fields: &[&str],
        visitor: &mut dyn FnMut(DocId, bool, &[&Value]) -> bool,
    ) -> StorageBackendResult<()> {
        self.0
            .for_each_fields_multi_ref_with_presence(doc_ids, fields, visitor)
    }

    fn get_shared_fields(
        &self,
        doc_ids: &[DocId],
        fields: &[&str],
    ) -> StorageBackendResult<Option<Vec<Option<SharedDocumentRow>>>> {
        self.0.get_shared_fields(doc_ids, fields)
    }

    fn get_fields_bulk(
        &self,
        doc_ids: &[DocId],
        field: &str,
    ) -> StorageBackendResult<BTreeMap<DocId, Value>> {
        self.0.get_fields_bulk(doc_ids, field)
    }

    fn has_value(&self, field: &str, value: &Value) -> StorageBackendResult<bool> {
        self.0.has_value(field, value)
    }

    fn find_doc_id_by_fields(
        &self,
        fields: &[String],
        values: &[Value],
    ) -> StorageBackendResult<Option<DocId>> {
        self.0.find_doc_id_by_fields(fields, values)
    }

    fn eval_path(
        &self,
        doc_id: DocId,
        path: &[PathSegment],
    ) -> StorageBackendResult<Option<Value>> {
        self.0.eval_path(doc_id, path)
    }

    fn doc_ids(&self) -> StorageBackendResult<Vec<DocId>> {
        self.0.doc_ids()
    }

    fn next_doc_id(&self, after: Option<DocId>) -> StorageBackendResult<Option<DocId>> {
        self.0.next_doc_id(after)
    }

    fn next_doc_ids(&self, after: Option<DocId>, limit: usize) -> StorageBackendResult<Vec<DocId>> {
        self.0.next_doc_ids(after, limit)
    }

    fn next_shared_fields(
        &self,
        after: Option<DocId>,
        limit: usize,
        fields: &[&str],
    ) -> StorageBackendResult<Option<Vec<(DocId, SharedDocumentRow)>>> {
        self.0.next_shared_fields(after, limit, fields)
    }

    fn for_each_next_fields(
        &self,
        after: Option<DocId>,
        limit: usize,
        fields: &[&str],
        visitor: &mut dyn FnMut(DocId, &[&Value]) -> bool,
    ) -> StorageBackendResult<Option<usize>> {
        self.0.for_each_next_fields(after, limit, fields, visitor)
    }

    fn max_doc_id(&self) -> StorageBackendResult<DocId> {
        self.0.max_doc_id()
    }

    fn len(&self) -> StorageBackendResult<usize> {
        self.0.len()
    }

    fn is_empty(&self) -> StorageBackendResult<bool> {
        self.0.is_empty()
    }

    fn iter_all(&self) -> StorageBackendResult<Box<dyn Iterator<Item = (DocId, Document)> + '_>> {
        self.0.iter_all()
    }

    fn snapshot(&self) -> StorageBackendResult<Arc<dyn DocumentStore>> {
        Ok(Arc::new(self.clone()))
    }
}
