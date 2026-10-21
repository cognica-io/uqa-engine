//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Freeze stored keys and selected identities before evaluating only missing physical keys.

use super::{rebuild::IndexDocuments, IndexExpressionContext, PhysicalIndexDefinitions};
use crate::{
    catalog::index::value::{ValueIndexBuilder, ValueIndexCache},
    mutation::candidate_ids::{CandidateIds, CapturedCandidateIds},
    physical::physical_exec_error,
    SharedSpill,
};
use uqa_core::{DocId, Value};
use uqa_sql::SQLError;
use uqa_storage::{
    document_store::read_document_ids, read_control::StorageReadControl, StorageBackendError,
    StorageBackendResult, ValueIndexKey,
};

mod capture;
mod projection;
use capture::{decode, Rows};

pub type StoredKeyVisitor<'a> = dyn FnMut(DocId, Value) -> StorageBackendResult<()> + 'a;

pub struct ValueIndexHydration<'a> {
    pub documents: &'a dyn IndexDocuments,
    pub definitions: &'a PhysicalIndexDefinitions,
    pub expressions: IndexExpressionContext<'a>,
    pub table: &'a str,
    pub field: &'a ValueIndexKey,
    pub work_mem_bytes: usize,
    pub control: &'a StorageReadControl,
}

fn storage(error: StorageBackendError) -> SQLError {
    crate::storage_errors::storage_error("hydrate stored index", &error)
}

impl ValueIndexHydration<'_> {
    /// The provider's complete read precedes identity capture, and all identity capture precedes expression evaluation. False means an unbuilt index and must not invoke its visitor. Stored expression keys are never reevaluated.
    pub fn build(
        &self,
        carried: bool,
        capture: impl FnOnce(&mut StoredKeyVisitor<'_>) -> StorageBackendResult<bool>,
    ) -> Result<ValueIndexCache, SQLError> {
        let persisted = self.capture_stored(capture)?;
        let ids = {
            let documents = self.documents.read();
            CandidateIds::from_pages(
                self.work_mem_bytes / 4,
                self.control.cancellation(),
                |after, limit, control| {
                    read_document_ids(&***documents, after, limit, control).map_err(storage)
                },
            )?
        };
        let mut output = ValueIndexBuilder::new(self.field.name(), carried, self.work_mem_bytes)?;
        let missing = self.reconcile(&persisted, &ids, &mut output)?;
        let mut chunk = Vec::with_capacity(256);
        for item in missing.read_rows()? {
            chunk.push(item?.1);
            if chunk.len() == 256 {
                self.project_chunk(&chunk, &mut output)?;
                chunk.clear();
            }
        }
        if !chunk.is_empty() {
            self.project_chunk(&chunk, &mut output)?;
        }
        output.finish(self.control)
    }

    fn capture_stored(
        &self,
        capture: impl FnOnce(&mut StoredKeyVisitor<'_>) -> StorageBackendResult<bool>,
    ) -> Result<SharedSpill, SQLError> {
        let mut rows = Rows::new(self.work_mem_bytes / 4)?;
        let mut previous = None;
        let captured = capture(&mut |id, value| {
            self.control.check()?;
            if previous.is_some_and(|previous| id <= previous) {
                return Err(StorageBackendError::Other(
                    "stored index identities do not advance".into(),
                ));
            }
            rows.push(id, value).map_err(|error| {
                StorageBackendError::backend("capture evaluated index key", error)
            })?;
            previous = Some(id);
            Ok(())
        })
        .map_err(storage)?;
        if !captured && previous.is_some() {
            return Err(SQLError::Internal(
                "unbuilt index supplied persisted entries".into(),
            ));
        }
        rows.finish()
    }

    fn reconcile(
        &self,
        persisted: &SharedSpill,
        ids: &CapturedCandidateIds,
        output: &mut ValueIndexBuilder,
    ) -> Result<CapturedCandidateIds, SQLError> {
        let mut missing =
            CandidateIds::new(self.work_mem_bytes / 4, self.control.cancellation().clone());
        let mut values = persisted
            .read_rows()
            .map_err(physical_exec_error)?
            .map(|row| decode(row.map_err(physical_exec_error)?.row));
        let mut current = values.next().transpose()?;
        for item in ids.read_rows()? {
            let (_, id) = item?;
            while current.as_ref().is_some_and(|(stored, _)| *stored < id) {
                current = values.next().transpose()?;
            }
            if current.as_ref().is_some_and(|(stored, _)| *stored == id) {
                let (_, value) = current.take().expect("matching stored identity");
                output.push(id, value, self.control)?;
                current = values.next().transpose()?;
            } else {
                missing.push(0, id)?;
            }
        }
        missing.finish()
    }
}

#[cfg(test)]
mod tests;
