//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Compound document reads keep keys, payloads, and projections on the supplied boundary.

use crate::document_store::Document;
use crate::key_value::codec::{
    decode_retained_stored_document_value, document_key, document_key_prefix, other_error,
};
use crate::key_value::view::for_each_key;
use crate::key_value::KeyValueRead;
use crate::{RetainedStoredDocument, StorageBackendResult, StoredDocument};
use std::collections::BTreeMap;
use uqa_core::{DocId, Value};

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
        let prefix = document_key_prefix(self.table)?;
        let after = after.map(|id| document_key(self.table, id)).transpose()?;
        let mut ids = Vec::new();
        self.read.visit_keys_after(
            &prefix,
            after.as_deref(),
            limit,
            self.read.control(),
            &mut |key| {
                ids.push(decode_id(&prefix, key)?);
                Ok(())
            },
        )?;
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
