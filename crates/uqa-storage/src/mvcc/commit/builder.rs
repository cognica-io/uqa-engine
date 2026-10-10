//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Collect the writes of a batch derived from another.

use uqa_core::memory::BudgetedVec;

use crate::mvcc::overlay::run::SpilledRunWriter;
use crate::mvcc::{
    PrivateRecordChanges, PrivateRecordRevision, PrivateRevisionScope, VersionResult,
};
use crate::read_control::StorageReadControl;

use super::{PreparedRecordCommit, PreparedRecordWrite};

enum Target {
    Resident(BudgetedVec<PreparedRecordWrite>),
    Spilled {
        writer: Box<SpilledRunWriter>,
        identity: PrivateRecordRevision,
    },
}

/// Collects the writes of a batch derived from another in the order of the batch it derives from: in memory for a batch in memory, and in a spilled run for a spilled batch, whose writes are ordered by key.
pub(in crate::mvcc) struct PreparedWritesBuilder {
    target: Target,
}

impl PreparedWritesBuilder {
    /// A builder for the writes derived from `original`.
    pub(in crate::mvcc) fn like(
        original: &PreparedRecordCommit,
        control: &StorageReadControl,
    ) -> VersionResult<Self> {
        let target = match &original.writes {
            super::writes::PreparedWrites::Resident(writes) => {
                let mut derived = BudgetedVec::new(control.memory());
                derived.reserve(writes.len())?;
                Target::Resident(derived)
            }
            super::writes::PreparedWrites::Spilled(run) => Target::Spilled {
                writer: Box::new(SpilledRunWriter::new(
                    run.len(),
                    run.key_bytes(),
                    control.memory(),
                )?),
                identity: PrivateRecordRevision::allocate()?,
            },
        };
        Ok(Self { target })
    }

    pub(in crate::mvcc) fn push(
        &mut self,
        write: PreparedRecordWrite,
        control: &StorageReadControl,
    ) -> VersionResult<()> {
        match &mut self.target {
            Target::Resident(writes) => writes.push(write).map_err(Into::into),
            Target::Spilled { writer, identity } => writer.push(
                write.key(),
                write.expected(),
                write.kind(),
                *identity,
                write.value(),
                control,
            ),
        }
    }

    /// Adopt the derived writes as an independent private root. A sorted spilled input becomes one run directly, without restaging and repeatedly merging its values.
    pub(in crate::mvcc) fn finish_changes(
        self,
        scope: Option<PrivateRevisionScope>,
        control: &StorageReadControl,
    ) -> VersionResult<PrivateRecordChanges> {
        match self.target {
            Target::Resident(writes) => {
                let changes = PrivateRecordChanges::with_revision_scope(control.memory(), scope);
                for write in &*writes {
                    changes.apply_owned(std::slice::from_ref(write), control)?;
                }
                Ok(changes)
            }
            Target::Spilled { writer, identity } => {
                control.check()?;
                PrivateRecordChanges::from_spilled_run(writer.finish()?, identity, scope, control)
            }
        }
    }

    /// The derived batch, fingerprinted as any other.
    pub(in crate::mvcc) fn finish(
        self,
        control: &StorageReadControl,
    ) -> VersionResult<PreparedRecordCommit> {
        match self.target {
            Target::Resident(writes) => PreparedRecordCommit::from_unique_owned(writes, control),
            Target::Spilled { writer, .. } => match writer.finish()? {
                Some(run) => PreparedRecordCommit::from_spilled_run(run, control),
                None => PreparedRecordCommit::from_unique_owned(
                    BudgetedVec::new(control.memory()),
                    control,
                ),
            },
        }
    }
}
