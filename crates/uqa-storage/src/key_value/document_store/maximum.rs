//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bounded identity input visits live keys and tombstones on one retained record view.

use super::{read::decode_id, KeyValueDocumentStore, Source};
use crate::{
    document_store::maximum::{self, MaximumDocIdInput, MaximumDocIdSource, MaximumDocIdStep},
    key_value::{codec::document_key_prefix_controlled, KeyValueRead},
    read_control::StorageReadControl,
    StorageBackendError, StorageBackendResult,
};
use std::sync::Arc;
use uqa_core::{memory::BudgetedVec, DocId};

#[cfg(test)]
mod tests;

pub(super) fn begin(
    documents: &KeyValueDocumentStore,
    maximum_key_bytes: usize,
    control: &StorageReadControl,
) -> StorageBackendResult<MaximumDocIdInput> {
    maximum::validate_limit(maximum_key_bytes, control)?;
    if let Source::Live(store) = &documents.source {
        if !store.transaction_model().is_versioned() {
            return Err(StorageBackendError::Other(
                "retained document maximum requires a versioned read owner".into(),
            ));
        }
    }
    let size = documents
        .table
        .len()
        .checked_add(5)
        .ok_or(uqa_core::memory::MemoryError::SizeOverflow)?;
    control.check_value_size(size, maximum_key_bytes)?;
    let prefix = document_key_prefix_controlled(&documents.table, control)?;
    let read = match &documents.source {
        Source::Live(_) => documents.read(|view| view.read.retain(&[&prefix]))?,
        Source::Retained(read) => read.clone(),
    };
    read.control().check()?;
    MaximumDocIdInput::new(
        Input {
            read,
            prefix,
            after: None,
            maximum: 0,
            maximum_key_bytes,
        },
        control,
    )
}

struct Input {
    read: Arc<dyn KeyValueRead + Send + Sync>,
    prefix: BudgetedVec<u8>,
    after: Option<BudgetedVec<u8>>,
    maximum: DocId,
    maximum_key_bytes: usize,
}

impl MaximumDocIdSource for Input {
    fn advance(&mut self, control: &StorageReadControl) -> StorageBackendResult<MaximumDocIdStep> {
        control.check()?;
        self.read.control().check()?;
        let mut next = None;
        let mut failure = None;
        let scanned = self.read.visit_record_keys_after(
            &self.prefix,
            self.after.as_deref(),
            1,
            control,
            &mut |key, live| {
                if failure.is_some() {
                    return Err(StorageBackendError::Other(
                        "document maximum visitor failed".into(),
                    ));
                }
                let result = (|| {
                    control.check()?;
                    self.read.control().check()?;
                    control.check_value_size(key.len(), self.maximum_key_bytes)?;
                    if next.is_some()
                        || !key.starts_with(&self.prefix)
                        || self.after.as_deref().is_some_and(|after| key <= after)
                    {
                        return Err(StorageBackendError::Other(
                            "document maximum scan exceeds its selected range".into(),
                        ));
                    }
                    if live {
                        self.maximum = self.maximum.max(decode_id(&self.prefix, key)?);
                    }
                    let mut owned = BudgetedVec::new(control.memory());
                    owned.extend_from_slice(key)?;
                    next = Some(owned);
                    Ok(())
                })();
                result.map_err(|error| {
                    failure = Some(error);
                    StorageBackendError::Other("document maximum visitor failed".into())
                })
            },
        );
        if let Some(error) = failure {
            return Err(error);
        }
        scanned?;
        control.check()?;
        self.read.control().check()?;
        if let Some(key) = next {
            self.after = Some(key);
            Ok(MaximumDocIdStep::Pending)
        } else {
            Ok(MaximumDocIdStep::Complete(self.maximum))
        }
    }
}
