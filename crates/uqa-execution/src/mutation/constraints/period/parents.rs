//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retain temporal parent lock targets in their original physical scan order.

use crate::mutation::candidate::PhysicalDocumentIdentity;
use crate::mutation::candidate_ids::{CandidateIds, CapturedCandidateIds};
use uqa_core::{CancellationToken, DocId};
use uqa_sql::SQLError;

pub(super) struct ParentIds {
    tables: Vec<String>,
    ids: CandidateIds,
}

pub(crate) struct PeriodParentIds {
    tables: Vec<String>,
    ids: CapturedCandidateIds,
}

impl ParentIds {
    pub(super) fn new(tables: Vec<String>, bytes: usize, cancellation: CancellationToken) -> Self {
        Self {
            tables,
            ids: CandidateIds::new(bytes, cancellation),
        }
    }

    pub(super) fn push(&mut self, table: usize, id: DocId) -> Result<(), SQLError> {
        self.ids.push(table, id)
    }

    pub(super) fn finish(self) -> Result<PeriodParentIds, SQLError> {
        Ok(PeriodParentIds {
            tables: self.tables,
            ids: self.ids.finish()?,
        })
    }
}

impl PeriodParentIds {
    pub(crate) fn read(
        &self,
    ) -> Result<impl Iterator<Item = Result<PhysicalDocumentIdentity, SQLError>> + '_, SQLError>
    {
        Ok(self.ids.read_rows()?.map(|item| {
            let (table, doc_id) = item?;
            let table = self.tables.get(table).ok_or_else(|| {
                SQLError::Internal("PERIOD parent lost its physical table".into())
            })?;
            Ok(PhysicalDocumentIdentity {
                table: table.clone(),
                doc_id,
            })
        }))
    }
}
