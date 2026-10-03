//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The final writes of a prepared commit, in memory or in a spilled run.

use std::sync::Arc;

use uqa_core::memory::BudgetedVec;

use crate::mvcc::overlay::run::SpilledRun;

use super::cursor::PreparedWriteCursor;
use super::PreparedRecordWrite;

/// The writes of a commit: in memory, in the order they were supplied, or, for a transaction larger than its allowance, in a spilled run ordered by key.
pub(in crate::mvcc) enum PreparedWrites {
    Resident(BudgetedVec<PreparedRecordWrite>),
    Spilled(Arc<SpilledRun>),
}

impl PreparedWrites {
    pub(in crate::mvcc) fn len(&self) -> usize {
        match self {
            Self::Resident(writes) => writes.len(),
            Self::Spilled(run) => usize::try_from(run.len()).unwrap_or(usize::MAX),
        }
    }

    pub(in crate::mvcc) fn cursor(&self) -> PreparedWriteCursor<'_> {
        match self {
            Self::Resident(writes) => PreparedWriteCursor::resident(writes),
            Self::Spilled(run) => PreparedWriteCursor::spilled(run),
        }
    }

    /// Whether some write is not canonical: such a batch has derived effects to resolve.
    pub(in crate::mvcc) fn typed(&self) -> bool {
        match self {
            Self::Resident(writes) => writes
                .iter()
                .any(|write| write.kind() != super::RecordWriteKind::Canonical),
            Self::Spilled(run) => run.typed(),
        }
    }
}
