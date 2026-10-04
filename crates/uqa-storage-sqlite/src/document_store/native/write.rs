//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Evaluated document, BLOB and dependent B-tree mutations form one atomic private batch.

use rusqlite::types::ValueRef;
use uqa_storage::{document_store::identifiers::DocumentIdNamespace, KeyValueBatch};

use super::NativeDocumentRead;
use crate::document_store::{
    encode_document_blobs, sqlite_doc_id, BTreeMap, DocId, Document, DocumentMetadata,
    SQLiteResult, Value,
};
use crate::mvcc::native::{NativeRecordFamily as Family, NativeRecordOwner};

impl NativeDocumentRead<'_> {
    /// Stage a document. `unused` names the namespace in which no document ever had `doc_id`. While the table's rows are stored under that namespace, the document has no body and no BLOB rows to replace, so its rows are written without reading what was there.
    pub(crate) fn put(
        &self,
        batch: &mut dyn KeyValueBatch,
        doc_id: DocId,
        fields: &Document,
        metadata: DocumentMetadata,
        unused: Option<DocumentIdNamespace>,
    ) -> SQLiteResult<()> {
        let id = sqlite_doc_id(doc_id)?;
        let owner = self.snapshot.ensure_table_owner(self.table, batch)?;
        let unused = unused.is_some_and(|namespace| owner.stores(namespace));
        let (fields, blobs) = encode_document_blobs(
            fields
                .iter()
                .filter(|(_, value)| !matches!(value, Value::Null))
                .map(|(field, value)| (field.clone(), value.clone()))
                .collect(),
        )?;
        if !unused {
            self.snapshot.delete_prefix(
                batch,
                Family::DocumentBlobs,
                owner,
                &[ValueRef::Integer(id)],
            )?;
        }
        self.stage_body(batch, owner, id, &fields, metadata, unused)?;
        for (field, bytes) in blobs {
            self.stage_blob(batch, owner, id, &field, &bytes, unused)?;
        }
        Ok(())
    }

    pub(crate) fn patch(
        &self,
        batch: &mut dyn KeyValueBatch,
        doc_id: DocId,
        updates: &BTreeMap<String, Value>,
    ) -> SQLiteResult<bool> {
        if updates.is_empty() {
            return Ok(true);
        }
        let Some(document) = self.body(doc_id)? else {
            return Ok(false);
        };
        let owner = self.owner.expect("visible document has an owner");
        let id = sqlite_doc_id(doc_id)?;
        let (mut fields, metadata) = document.into_parts();
        let (encoded, blobs) = encode_document_blobs(
            updates
                .iter()
                .filter(|(_, value)| !matches!(value, Value::Null))
                .map(|(field, value)| (field.clone(), value.clone()))
                .collect(),
        )?;
        for field in updates.keys() {
            fields.remove(field);
            self.snapshot.delete_prefix(
                batch,
                Family::DocumentBlobs,
                owner,
                &[ValueRef::Integer(id), ValueRef::Text(field.as_bytes())],
            )?;
        }
        fields.extend(encoded);
        self.stage_body(batch, owner, id, &fields, metadata, false)?;
        for (field, bytes) in blobs {
            self.stage_blob(batch, owner, id, &field, &bytes, false)?;
        }
        Ok(true)
    }

    fn stage_body(
        &self,
        batch: &mut dyn KeyValueBatch,
        owner: NativeRecordOwner,
        id: i64,
        fields: &Document,
        metadata: DocumentMetadata,
        unused: bool,
    ) -> SQLiteResult<()> {
        let body = serde_json::to_string(fields)?;
        let NativeRecordOwner::Object {
            identity,
            generation,
        } = owner
        else {
            return Err(crate::SQLiteError::StorageBackend(
                "document allocation requires a table owner".into(),
            ));
        };
        uqa_storage::document_store::identifiers::observe_document_id(
            batch,
            identity,
            generation,
            super::super::document_id_from_sqlite(id)?,
        )?;
        let row = [
            ValueRef::Text(self.table.as_bytes()),
            ValueRef::Integer(id),
            ValueRef::Text(body.as_bytes()),
            metadata
                .tuple_xmin()
                .map_or(ValueRef::Null, |xmin| ValueRef::Integer(i64::from(xmin))),
        ];
        if unused {
            self.snapshot
                .put_unused_row(batch, Family::Documents, owner, &row)
        } else {
            self.snapshot.put_row(batch, Family::Documents, owner, &row)
        }
    }

    fn stage_blob(
        &self,
        batch: &mut dyn KeyValueBatch,
        owner: NativeRecordOwner,
        id: i64,
        field: &str,
        bytes: &[u8],
        unused: bool,
    ) -> SQLiteResult<()> {
        let row = [
            ValueRef::Text(self.table.as_bytes()),
            ValueRef::Integer(id),
            ValueRef::Text(field.as_bytes()),
            ValueRef::Blob(bytes),
        ];
        if unused {
            self.snapshot
                .put_unused_row(batch, Family::DocumentBlobs, owner, &row)
        } else {
            self.snapshot
                .put_row(batch, Family::DocumentBlobs, owner, &row)
        }
    }

    pub(crate) fn delete(&self, batch: &mut dyn KeyValueBatch, doc_id: DocId) -> SQLiteResult<()> {
        let id = sqlite_doc_id(doc_id)?;
        let Some(owner) = self.owner else {
            return Ok(());
        };
        self.snapshot.delete_prefix(
            batch,
            Family::DocumentBlobs,
            owner,
            &[ValueRef::Integer(id)],
        )?;
        self.snapshot
            .delete_prefix(batch, Family::Documents, owner, &[ValueRef::Integer(id)])?;
        // Native SQLite's document deletion also cascades to both TEXT and BLOB B-tree namespaces.
        crate::btree_index::delete_native_document_entries(self.snapshot, batch, owner, id)?;
        Ok(())
    }

    pub(crate) fn clear(&self, batch: &mut dyn KeyValueBatch) -> SQLiteResult<()> {
        if let Some(owner) = self.owner {
            for family in [
                Family::Documents,
                Family::DocumentBlobs,
                Family::BtreeIndexEntries,
            ] {
                self.snapshot.delete_prefix(batch, family, owner, &[])?;
            }
        }
        Ok(())
    }
}
