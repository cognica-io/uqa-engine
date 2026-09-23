//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Compound reads hydrate selected BLOBs from the same retained record view as the document body.

#[cfg(test)]
mod tests;

use rusqlite::types::ValueRef;
use uqa_core::memory::BudgetedVec;
use uqa_storage::mvcc::VersionError;

use super::NativeDocumentRead;
use crate::document_store::{
    document_id_from_sqlite, sqlite_doc_id, BTreeMap, DocId, DocumentMetadata, SQLiteError,
    SQLiteResult, StoredDocument, Value,
};
use crate::mvcc::native::{NativeRecordFamily as Family, NativeRecordIdentity};

impl NativeDocumentRead<'_> {
    pub(crate) fn body(&self, doc_id: DocId) -> SQLiteResult<Option<StoredDocument>> {
        Ok(self
            .retained_body(doc_id)?
            .map(uqa_storage::RetainedStoredDocument::into_stored))
    }

    pub(crate) fn get_stored(&self, doc_id: DocId) -> SQLiteResult<Option<StoredDocument>> {
        Ok(self
            .retained(doc_id, None)?
            .map(uqa_storage::RetainedStoredDocument::into_stored))
    }

    pub(crate) fn get_field(&self, doc_id: DocId, field: &str) -> SQLiteResult<Option<Value>> {
        Ok(self
            .retained(doc_id, Some(&[field]))?
            .and_then(|document| document.into_stored().into_fields().remove(field)))
    }

    pub(crate) fn metadata(&self, doc_id: DocId) -> SQLiteResult<Option<DocumentMetadata>> {
        let id = sqlite_doc_id(doc_id)?;
        let Some(owner) = self.owner else {
            return Ok(None);
        };
        self.snapshot
            .read_row(Family::Documents, owner, &[ValueRef::Integer(id)], |row| {
                metadata(row[3], self.table, doc_id)
            })
    }

    pub(crate) fn contains(&self, doc_id: DocId) -> SQLiteResult<bool> {
        let id = sqlite_doc_id(doc_id)?;
        let Some(owner) = self.owner else {
            return Ok(false);
        };
        let key = NativeRecordIdentity::new(Family::Documents, owner)?
            .encode_key(&[ValueRef::Integer(id)], &self.snapshot.control)?;
        Ok(self
            .snapshot
            .view
            .metadata(&key, &self.snapshot.control)?
            .is_some_and(|record| record.live))
    }

    pub(crate) fn stored_many(
        &self,
        ids: &[DocId],
    ) -> SQLiteResult<BTreeMap<DocId, StoredDocument>> {
        let mut out = BTreeMap::new();
        for &id in ids {
            if let Some(document) = self.get_stored(id)? {
                out.insert(id, document);
            }
        }
        Ok(out)
    }

    pub(crate) fn fields_bulk(
        &self,
        ids: &[DocId],
        field: &str,
    ) -> SQLiteResult<BTreeMap<DocId, Value>> {
        let mut out = BTreeMap::new();
        for &id in ids {
            out.insert(id, self.get_field(id, field)?.unwrap_or(Value::Null));
        }
        Ok(out)
    }

    pub(crate) fn fields_multi(
        &self,
        ids: &[DocId],
        fields: &[&str],
    ) -> SQLiteResult<BTreeMap<DocId, Vec<Value>>> {
        let mut out = BTreeMap::new();
        if fields.is_empty() {
            return Ok(out);
        }
        for &id in ids {
            let Some(document) = self.retained(id, Some(fields))? else {
                continue;
            };
            let mut values = Vec::new();
            values.try_reserve_exact(fields.len()).map_err(|error| {
                super::super::allocation_error("native projected fields", error)
            })?;
            for &field in fields {
                values.push(document.fields().get(field).cloned().unwrap_or(Value::Null));
            }
            out.insert(id, values);
        }
        Ok(out)
    }

    fn visit_ids(
        &self,
        after: Option<DocId>,
        limit: usize,
        mut visit: impl FnMut(DocId) -> SQLiteResult<()>,
    ) -> SQLiteResult<()> {
        if limit == 0 {
            return Ok(());
        }
        let after = after.map(sqlite_doc_id).transpose()?;
        let Some(owner) = self.owner else {
            return Ok(());
        };
        let identity = NativeRecordIdentity::new(Family::Documents, owner)?;
        let prefix = identity.encode_prefix(&[], &self.snapshot.control)?;
        let after = after
            .map(|id| identity.encode_key(&[ValueRef::Integer(id)], &self.snapshot.control))
            .transpose()?;
        let mut count = 0;
        self.snapshot.view.visit_keys(
            &prefix,
            after.as_deref(),
            usize::MAX,
            &self.snapshot.control,
            &mut |key, record| {
                if record.live {
                    NativeRecordIdentity::visit_key_components(
                        key,
                        &self.snapshot.control,
                        |_, value| {
                            let ValueRef::Integer(id) = value else {
                                return Err(VersionError::InvalidEncoding(
                                    "native document key must be integer",
                                ));
                            };
                            let id = document_id_from_sqlite(id)
                                .map_err(|error| VersionError::Storage(error.into()))?;
                            visit(id).map_err(|error| VersionError::Storage(error.into()))
                        },
                    )?;
                    count += 1;
                }
                Ok(count < limit)
            },
        )?;
        Ok(())
    }

    pub(crate) fn ids(&self, after: Option<DocId>, limit: usize) -> SQLiteResult<Vec<DocId>> {
        let (ids, _memory) = self.id_page(after, limit)?.into_parts();
        Ok(ids)
    }

    fn id_page(&self, after: Option<DocId>, limit: usize) -> SQLiteResult<BudgetedVec<DocId>> {
        self.snapshot.control.check()?;
        let mut ids = BudgetedVec::new(self.snapshot.control.memory());
        self.visit_ids(after, limit, |id| {
            self.snapshot.control.check()?;
            ids.push(id)?;
            Ok(())
        })?;
        Ok(ids)
    }

    pub(in crate::document_store) fn visit_next_ids(
        &self,
        after: Option<DocId>,
        limit: usize,
        visitor: &mut dyn FnMut(DocId, &[&Value]) -> bool,
    ) -> SQLiteResult<usize> {
        let ids = self.id_page(after, limit)?;
        let mut visited = 0;
        for id in ids.iter().copied() {
            self.snapshot.control.check()?;
            visited += 1;
            let keep_going = visitor(id, &[]);
            self.snapshot.control.check()?;
            if !keep_going {
                break;
            }
        }
        self.snapshot.control.check()?;
        Ok(visited)
    }

    pub(crate) fn len(&self) -> SQLiteResult<usize> {
        let mut count: usize = 0;
        self.visit_ids(None, usize::MAX, |_| {
            count = count.checked_add(1).ok_or_else(|| {
                SQLiteError::StorageBackend("native document count overflow".into())
            })?;
            Ok(())
        })?;
        Ok(count)
    }

    pub(crate) fn max_doc_id(&self) -> SQLiteResult<DocId> {
        let mut last = 0;
        self.visit_ids(None, usize::MAX, |id| {
            last = id;
            Ok(())
        })?;
        Ok(last)
    }

    pub(crate) fn find(&self, field: &str, value: &Value) -> SQLiteResult<Option<DocId>> {
        self.find_matching(|id| Ok(self.get_field(id, field)?.as_ref() == Some(value)))
    }

    pub(crate) fn find_fields(
        &self,
        fields: &[String],
        values: &[Value],
    ) -> SQLiteResult<Option<DocId>> {
        if fields.is_empty() || fields.len() != values.len() {
            return Ok(None);
        }
        self.find_matching(|id| {
            for (field, value) in fields.iter().zip(values) {
                if self.get_field(id, field)?.unwrap_or(Value::Null) != *value {
                    return Ok(false);
                }
            }
            Ok(true)
        })
    }

    fn find_matching(
        &self,
        mut matches: impl FnMut(DocId) -> SQLiteResult<bool>,
    ) -> SQLiteResult<Option<DocId>> {
        // Release key-visitor callbacks before reading payloads on the same persistence pool.
        let mut after = None;
        loop {
            let page = self.ids(after, 128)?;
            if page.is_empty() {
                return Ok(None);
            }
            for id in page {
                if matches(id)? {
                    return Ok(Some(id));
                }
                after = Some(id);
            }
        }
    }
}

pub(super) fn metadata(
    value: ValueRef<'_>,
    table: &str,
    doc_id: DocId,
) -> SQLiteResult<DocumentMetadata> {
    if value == ValueRef::Null {
        return Ok(DocumentMetadata::default());
    }
    let xmin = value
        .as_i64()
        .ok()
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| {
            SQLiteError::StorageBackend(format!(
                "document `{table}` row {doc_id} has an out-of-range tuple xmin"
            ))
        })?;
    Ok(DocumentMetadata::with_tuple_xmin(xmin))
}
