//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native maximum lookup keeps its reverse key order and yields after each deleted slot.

use super::{NativeDocumentRead, NativeSnapshot, SQLiteDocumentStore};
use crate::{
    document_store::document_id_from_sqlite,
    mvcc::native::{NativeRecordFamily as Family, NativeRecordIdentity},
};
use rusqlite::types::ValueRef;
use std::sync::Arc;
use uqa_core::memory::BudgetedVec;
use uqa_storage::{
    document_store::maximum::{self, MaximumDocIdInput, MaximumDocIdSource, MaximumDocIdStep},
    mvcc::VersionError,
    read_control::StorageReadControl,
    StorageBackendError, StorageBackendResult,
};

pub(in crate::document_store) fn begin(
    documents: &SQLiteDocumentStore,
    maximum_key_bytes: usize,
    control: &StorageReadControl,
) -> StorageBackendResult<MaximumDocIdInput> {
    maximum::validate_limit(maximum_key_bytes, control)?;
    let snapshot = match &documents.retained {
        Some(snapshot) => snapshot.clone(),
        None => documents.conn.native_snapshot()?.ok_or_else(|| {
            StorageBackendError::Other("retained document maximum requires native records".into())
        })?,
    };
    snapshot.control.check()?;
    let Some(owner) = snapshot.table_owner_controlled(&documents.table, control)? else {
        return MaximumDocIdInput::empty(control);
    };
    let prefix = NativeRecordIdentity::new(Family::Documents, owner)
        .map_err(VersionError::into_storage_error)?
        .encode_prefix(&[], control)
        .map_err(VersionError::into_storage_error)?;
    control.check_value_size(prefix.len(), maximum_key_bytes)?;
    MaximumDocIdInput::new(
        Input {
            snapshot,
            prefix,
            before: BudgetedVec::new(control.memory()),
            maximum_key_bytes,
        },
        control,
    )
}

struct Input {
    snapshot: Arc<NativeSnapshot>,
    prefix: BudgetedVec<u8>,
    before: BudgetedVec<u8>,
    maximum_key_bytes: usize,
}

impl MaximumDocIdSource for Input {
    fn advance(&mut self, control: &StorageReadControl) -> StorageBackendResult<MaximumDocIdStep> {
        Ok(step(
            &self.snapshot,
            &self.prefix,
            &mut self.before,
            self.maximum_key_bytes,
            control,
        )?)
    }
}

impl NativeDocumentRead<'_> {
    pub(crate) fn max_doc_id(&self) -> super::SQLiteResult<uqa_core::DocId> {
        let control = &self.snapshot.control;
        control.check()?;
        let Some(owner) = self.owner else {
            return Ok(0);
        };
        let prefix =
            NativeRecordIdentity::new(Family::Documents, owner)?.encode_prefix(&[], control)?;
        let mut before = BudgetedVec::new(control.memory());
        loop {
            if let MaximumDocIdStep::Complete(value) =
                step(self.snapshot, &prefix, &mut before, usize::MAX, control)?
            {
                return Ok(value);
            }
        }
    }
}

fn step(
    snapshot: &NativeSnapshot,
    prefix: &[u8],
    before: &mut BudgetedVec<u8>,
    maximum_key_bytes: usize,
    control: &StorageReadControl,
) -> super::SQLiteResult<MaximumDocIdStep> {
    control.check()?;
    snapshot.control.check()?;
    let mut found = None;
    let mut preceding = BudgetedVec::new(control.memory());
    snapshot.view.visit_last_key(
        prefix,
        (!before.is_empty()).then_some(&**before),
        control,
        &mut |key, record| {
            control.check_value_size(key.len(), maximum_key_bytes)?;
            if record.live {
                NativeRecordIdentity::visit_key_components(key, control, |_, value| {
                    let ValueRef::Integer(id) = value else {
                        return Err(VersionError::InvalidEncoding(
                            "native document key must be integer",
                        ));
                    };
                    found = Some(
                        document_id_from_sqlite(id)
                            .map_err(|error| VersionError::Storage(error.into()))?,
                    );
                    Ok(())
                })?;
            } else {
                preceding.extend_from_slice(key)?;
            }
            Ok(false)
        },
    )?;
    control.check()?;
    snapshot.control.check()?;
    if let Some(id) = found {
        Ok(MaximumDocIdStep::Complete(id))
    } else if preceding.is_empty() {
        Ok(MaximumDocIdStep::Complete(0))
    } else {
        *before = preceding;
        Ok(MaximumDocIdStep::Pending)
    }
}
