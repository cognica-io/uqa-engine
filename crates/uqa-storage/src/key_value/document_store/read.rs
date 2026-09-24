//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Compound document reads keep keys, payloads, and projections on the supplied boundary.

#[cfg(test)]
mod tests;

use crate::document_store::Document;
use crate::key_value::codec::{
    decode_retained_stored_document_value, document_key, document_key_prefix,
    document_key_prefix_controlled, other_error,
};
use crate::key_value::view::for_each_key;
use crate::key_value::KeyValueRead;
use crate::read_control::StorageReadControl;
use crate::{RetainedStoredDocument, StorageBackendResult, StoredDocument};
use std::collections::BTreeMap;
use uqa_core::{memory::BudgetedVec, DocId, Value};

pub(super) struct Documents<'a> {
    pub(super) read: &'a dyn KeyValueRead,
    pub(super) table: &'a str,
}

fn decode_id(prefix: &[u8], key: &[u8]) -> StorageBackendResult<DocId> {
    let bytes = key
        .get(prefix.len()..)
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or_else(|| other_error("invalid document identity key"))?;
    Ok(u64::from_be_bytes(bytes))
}

impl Documents<'_> {
    pub(super) fn retained_many_controlled(
        &self,
        ids: &[DocId],
        control: &StorageReadControl,
    ) -> StorageBackendResult<crate::RetainedDocumentPage> {
        self.read.control().check()?;
        control.check()?;
        let mut page = BudgetedVec::new(control.memory());
        page.reserve(ids.len())?;
        for id in ids {
            self.read.control().check()?;
            control.check()?;
            let mut key = document_key_prefix_controlled(self.table, control)?;
            key.extend_from_slice(&id.to_be_bytes())?;
            let mut result = None;
            let mut failure = None;
            let scanned = self.read.visit_value_budgeted(&key, control, &mut |value| {
                if failure.is_some() {
                    return Err(other_error("document value visitor has already failed"));
                }
                let decoded = (|| {
                    self.read.control().check()?;
                    control.check()?;
                    if result.is_some() {
                        return Err(other_error(
                            "document value visitor returned more than one row",
                        ));
                    }
                    value
                        .map(|bytes| decode_retained_stored_document_value(bytes, control))
                        .transpose()
                })();
                match decoded {
                    Ok(row) => {
                        result = Some(row);
                        Ok(())
                    }
                    Err(error) => {
                        failure = Some(error);
                        Err(other_error("document value visitor failed"))
                    }
                }
            });
            if let Some(error) = failure {
                return Err(error);
            }
            scanned?;
            self.read.control().check()?;
            control.check()?;
            page.push(
                result.ok_or_else(|| other_error("document value visitor did not return a row"))?,
            )?;
        }
        self.read.control().check()?;
        control.check()?;
        Ok(page)
    }

    pub(super) fn get(&self, id: DocId) -> StorageBackendResult<Option<StoredDocument>> {
        self.get_retained(id)
            .map(|document| document.map(RetainedStoredDocument::into_stored))
    }

    pub(super) fn get_retained(
        &self,
        id: DocId,
    ) -> StorageBackendResult<Option<RetainedStoredDocument>> {
        let mut result = None;
        self.read
            .visit_value(&document_key(self.table, id)?, &mut |value| {
                result = value
                    .map(|value| decode_retained_stored_document_value(value, self.read.control()))
                    .transpose()?;
                Ok(())
            })?;
        Ok(result)
    }

    pub(super) fn contains(&self, id: DocId) -> StorageBackendResult<bool> {
        let key = document_key(self.table, id)?;
        let mut found = false;
        self.read
            .visit_keys_after(&key, None, 1, self.read.control(), &mut |candidate| {
                found = candidate == key;
                Ok(())
            })?;
        Ok(found)
    }

    pub(super) fn many(
        &self,
        ids: &[DocId],
    ) -> StorageBackendResult<BTreeMap<DocId, StoredDocument>> {
        let mut result = BTreeMap::new();
        for id in ids {
            if let Some(document) = self.get(*id)? {
                result.insert(*id, document);
            }
        }
        Ok(result)
    }

    pub(super) fn project(
        &self,
        ids: &[DocId],
        fields: &[&str],
    ) -> StorageBackendResult<BTreeMap<DocId, Vec<Value>>> {
        let mut result = BTreeMap::new();
        for id in ids {
            if fields.is_empty() {
                if self.contains(*id)? {
                    result.insert(*id, Vec::new());
                }
            } else if let Some(document) = self.get(*id)? {
                result.insert(
                    *id,
                    fields
                        .iter()
                        .map(|field| {
                            document
                                .fields()
                                .get(*field)
                                .cloned()
                                .unwrap_or(Value::Null)
                        })
                        .collect(),
                );
            }
        }
        Ok(result)
    }

    pub(super) fn ids(
        &self,
        after: Option<DocId>,
        limit: usize,
    ) -> StorageBackendResult<Vec<DocId>> {
        let (ids, _memory) = self.id_page(after, limit)?.into_parts();
        Ok(ids)
    }

    pub(super) fn id_page(
        &self,
        after: Option<DocId>,
        limit: usize,
    ) -> StorageBackendResult<BudgetedVec<DocId>> {
        self.id_page_controlled(after, limit, self.read.control())
    }

    pub(super) fn id_page_controlled(
        &self,
        after: Option<DocId>,
        limit: usize,
        control: &StorageReadControl,
    ) -> StorageBackendResult<BudgetedVec<DocId>> {
        control.check()?;
        self.read.control().check()?;
        let mut ids = BudgetedVec::new(control.memory());
        if limit == 0 {
            return Ok(ids);
        }
        let prefix = document_key_prefix_controlled(self.table, control)?;
        let mut cursor = BudgetedVec::new(control.memory());
        if let Some(id) = after {
            cursor.reserve(
                prefix
                    .len()
                    .checked_add(8)
                    .ok_or(uqa_core::memory::MemoryError::SizeOverflow)?,
            )?;
            cursor.extend_from_slice(&prefix)?;
            cursor.extend_from_slice(&id.to_be_bytes())?;
        }
        let mut failure = None;
        let mut previous = after;
        let scanned = self.read.visit_keys_after(
            &prefix,
            after.map(|_| &*cursor),
            limit,
            control,
            &mut |key| {
                if failure.is_some() {
                    return Err(other_error("document identity scan has already failed"));
                }
                let result = (|| {
                    control.check()?;
                    self.read.control().check()?;
                    if !key.starts_with(&prefix) || ids.len() == limit {
                        return Err(other_error(
                            "document identity scan exceeds its selected range",
                        ));
                    }
                    let id = decode_id(&prefix, key)?;
                    if previous.is_some_and(|previous| id <= previous) {
                        return Err(other_error(
                            "document identity scan does not advance in id order",
                        ));
                    }
                    ids.push(id)?;
                    previous = Some(id);
                    Ok(())
                })();
                result.map_err(|error| {
                    failure = Some(error);
                    other_error("document identity scan failed")
                })
            },
        );
        if let Some(error) = failure {
            return Err(error);
        }
        scanned?;
        control.check()?;
        self.read.control().check()?;
        Ok(ids)
    }

    pub(super) fn count(&self) -> StorageBackendResult<usize> {
        let prefix = document_key_prefix(self.table)?;
        let mut count = 0usize;
        self.read
            .visit_keys_after(&prefix, None, usize::MAX, self.read.control(), &mut |key| {
                decode_id(&prefix, key)?;
                count = count
                    .checked_add(1)
                    .ok_or_else(|| other_error("document count overflow"))?;
                Ok(())
            })?;
        Ok(count)
    }

    pub(super) fn find(
        &self,
        predicate: impl Fn(&Document) -> bool,
    ) -> StorageBackendResult<Option<DocId>> {
        let prefix = document_key_prefix(self.table)?;
        let mut found = None;
        for_each_key(self.read, &prefix, &mut |key| {
            let id = decode_id(&prefix, key)?;
            if let Some(document) = self.get_retained(id)? {
                if predicate(document.fields()) {
                    found = Some(id);
                }
            }
            Ok(found.is_none())
        })?;
        Ok(found)
    }

    pub(super) fn all(&self) -> StorageBackendResult<Vec<(DocId, Document)>> {
        let prefix = document_key_prefix(self.table)?;
        let mut result = Vec::new();
        self.read.visit_prefix(&prefix, &mut |key, value| {
            result.push((
                decode_id(&prefix, key)?,
                decode_retained_stored_document_value(value, self.read.control())?
                    .into_stored()
                    .into_fields(),
            ));
            Ok(())
        })?;
        Ok(result)
    }
}
