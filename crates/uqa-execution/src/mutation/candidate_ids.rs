//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Preserve the command's ordered target identities before any row effects, with bounded capture and sequential replay.

use uqa_core::{memory::MemoryBudget, CancellationToken, DocId};
use uqa_sql::SQLError;
use uqa_storage::{read_control::StorageReadControl, DocumentStore};

use crate::mutation::prepared::{decode_prepared_doc_id, encode_prepared_doc_id};
use crate::physical::physical_exec_error;
use crate::query::document_changes::{DocumentChanges, VisibleDocumentIds};
use crate::{Batch, PhysicalRow, RowSchema, SharedSpill, SpillBuffer};

const ID_PAGE_SIZE: usize = 128;
// Provider decode scratch and the visible-page merge have a fixed bound independent of relation size.
const ID_PAGE_MEMORY: usize = 64 * 1024;

pub(crate) struct CandidateIds {
    rows: SpillBuffer,
    schema: RowSchema,
    pending: Vec<PhysicalRow>,
    batch_limit: usize,
    cancellation: CancellationToken,
}

impl CandidateIds {
    pub(super) fn new(bytes: usize, cancellation: CancellationToken) -> Self {
        Self {
            rows: SpillBuffer::new(bytes),
            schema: RowSchema::with_internal_relation_types(
                uqa_sql::ast::InternalRelationId::allocate(),
                vec![None; 2],
            ),
            pending: Vec::new(),
            batch_limit: (bytes / 128).clamp(1, ID_PAGE_SIZE),
            cancellation,
        }
    }

    /// Table ordinals refer to the already bound hierarchy; no relation name is copied per candidate.
    pub(super) fn push(&mut self, table: usize, id: DocId) -> Result<(), SQLError> {
        self.cancellation.check()?;
        let table = u64::try_from(table).map_err(|_| invalid("table ordinal exceeds u64"))?;
        self.pending.push(PhysicalRow::from_values(vec![
            encode_prepared_doc_id(table),
            encode_prepared_doc_id(id),
        ]));
        if self.pending.len() == self.batch_limit {
            self.flush()?;
        }
        Ok(())
    }

    /// Both inputs come from the selected command snapshot, captured before opening the mutation overlay.
    pub(super) fn capture_table(
        &mut self,
        table: usize,
        source: &dyn DocumentStore,
        changes: &DocumentChanges,
    ) -> Result<(), SQLError> {
        self.capture_pages(table, |after, limit, control| {
            VisibleDocumentIds {
                source,
                changes,
                control,
            }
            .page(after, limit, control, true)
            .map_err(|error| {
                crate::storage_errors::storage_error("capture mutation target identities", &error)
            })
        })
    }

    /// Freeze the complete identity sequence before any caller reads or evaluates a row.
    pub(crate) fn from_pages(
        bytes: usize,
        cancellation: &CancellationToken,
        page: impl FnMut(
            Option<DocId>,
            usize,
            &StorageReadControl,
        ) -> Result<uqa_core::memory::BudgetedVec<DocId>, SQLError>,
    ) -> Result<CapturedCandidateIds, SQLError> {
        let mut ids = Self::new(bytes, cancellation.clone());
        ids.capture_pages(0, page)?;
        ids.finish()
    }

    fn capture_pages(
        &mut self,
        table: usize,
        mut page: impl FnMut(
            Option<DocId>,
            usize,
            &StorageReadControl,
        ) -> Result<uqa_core::memory::BudgetedVec<DocId>, SQLError>,
    ) -> Result<(), SQLError> {
        let cancellation = self.cancellation.clone();
        visit_identity_pages(&cancellation, &mut page, &mut |id| self.push(table, id))
    }

    fn flush(&mut self) -> Result<(), SQLError> {
        if !self.pending.is_empty() {
            self.rows
                .push(Batch::from_physical_rows(
                    self.schema.clone(),
                    std::mem::take(&mut self.pending),
                ))
                .map_err(physical_exec_error)?;
        }
        Ok(())
    }

    pub(super) fn finish(mut self) -> Result<CapturedCandidateIds, SQLError> {
        self.cancellation.check()?;
        self.flush()?;
        Ok(CapturedCandidateIds {
            rows: self
                .rows
                .into_shared(self.schema)
                .map_err(physical_exec_error)?,
            cancellation: self.cancellation,
        })
    }
}

pub(crate) struct CapturedCandidateIds {
    rows: SharedSpill,
    cancellation: CancellationToken,
}

impl CapturedCandidateIds {
    pub(super) fn is_empty(&self) -> bool {
        self.rows.rows() == 0
    }

    pub(crate) fn read_rows(
        &self,
    ) -> Result<impl Iterator<Item = Result<(usize, DocId), SQLError>> + '_, SQLError> {
        self.cancellation.check()?;
        Ok(self
            .rows
            .read_rows()
            .map_err(physical_exec_error)?
            .map(|row| {
                self.cancellation.check()?;
                let [table, id] = row
                    .map_err(physical_exec_error)?
                    .row
                    .into_physical_values()
                    .try_into()
                    .map_err(|_| invalid("candidate has an invalid width"))?;
                let table = decode_prepared_doc_id(table, "mutation target identities")?;
                let table =
                    usize::try_from(table).map_err(|_| invalid("table ordinal exceeds usize"))?;
                Ok((
                    table,
                    decode_prepared_doc_id(id, "mutation target identities")?,
                ))
            }))
    }
}

/// Visit the selected ordered identity pages before the caller begins any row evaluation.
pub(crate) fn visit_identity_pages(
    cancellation: &CancellationToken,
    mut page: impl FnMut(
        Option<DocId>,
        usize,
        &StorageReadControl,
    ) -> Result<uqa_core::memory::BudgetedVec<DocId>, SQLError>,
    visit: &mut dyn FnMut(DocId) -> Result<(), SQLError>,
) -> Result<(), SQLError> {
    let control = StorageReadControl::new(&MemoryBudget::new(ID_PAGE_MEMORY), cancellation);
    let mut after = None;
    loop {
        cancellation.check()?;
        let page = page(after, ID_PAGE_SIZE, &control)?;
        let Some(last) = page.last().copied() else {
            return Ok(());
        };
        after = Some(last);
        for &id in page.iter() {
            visit(id)?;
        }
    }
}

fn invalid(message: &str) -> SQLError {
    SQLError::Internal(format!("mutation target identities: {message}"))
}

#[cfg(test)]
mod tests;
