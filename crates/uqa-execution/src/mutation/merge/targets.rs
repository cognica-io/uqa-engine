//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The paired targets a MERGE locks, followed to the row versions a committed update chain moved them to.

use super::analysis::merge_target_lock_strength;
use crate::mutation::{
    candidate::PhysicalMutationLockTarget, locking::lock_physical_mutation_target,
    referential::ReferentialContext, row_membership::PhysicalRows,
};
use uqa_core::memory::MemoryBudget;
use uqa_sql::{plan::MergePlan, SQLError};

mod retained;
use super::codec::{MergePairKind, MergePairing};
use retained::{RecheckedTarget, TargetChanges};

/// The paired targets of a MERGE once locked.
pub(super) struct LockedMergeTargets {
    /// Whether a wait or a successor makes the statement requalify its matches.
    pub(super) recheck_matches: bool,
    changes: TargetChanges,
}

impl LockedMergeTargets {
    /// Return false only when a vanished target eliminates a source-missing pairing.
    pub(super) fn apply_to_pair(&mut self, pair: &mut MergePairing) -> Result<bool, SQLError> {
        let Some((table, id)) = pair.storage_table.as_deref().zip(pair.doc_id) else {
            return Ok(true);
        };
        match self.changes.lookup(table, id)? {
            Some(RecheckedTarget::Deleted) => match pair.kind {
                MergePairKind::Matched => {
                    pair.kind = MergePairKind::NotMatchedByTarget;
                    pair.storage_table = None;
                    pair.doc_id = None;
                    pair.target_document = None;
                }
                MergePairKind::NotMatchedBySource => return Ok(false),
                MergePairKind::NotMatchedByTarget => {}
            },
            Some(RecheckedTarget::Refreshed((table, id), document)) => {
                pair.storage_table = Some(table);
                pair.doc_id = Some(id);
                pair.target_document = Some(document);
            }
            None => {}
        }
        Ok(true)
    }
}

/// Lock the paired targets in relation/unsigned-identity order. Retain moved identities before refreshing the statement snapshot, then retain each latest document before executing any actions.
pub(super) fn lock_merge_targets<S: Clone + 'static>(
    referential: &ReferentialContext<'_, S>,
    stmt: &MergePlan,
    target_qualifier: &str,
    targets: PhysicalRows,
) -> Result<LockedMergeTargets, SQLError> {
    let mut recheck_matches = false;
    let memory = MemoryBudget::new(referential.constraints.memory.work_mem_bytes()?);
    let mut pending = TargetChanges::new(&memory, referential.constraints.cancellation.clone());
    targets.visit(&mut |storage_table, doc_id| {
        let target = lock_physical_mutation_target(
            referential.locking.session,
            storage_table,
            target_qualifier,
            doc_id,
            merge_target_lock_strength(referential.locking.catalog, stmt, storage_table),
        )?;
        match target {
            PhysicalMutationLockTarget::Present { identity, recheck } => {
                recheck_matches |= recheck;
                if recheck || identity.table != storage_table || identity.doc_id != doc_id {
                    pending.push(
                        (storage_table, doc_id),
                        Some((&identity.table, identity.doc_id)),
                        None,
                    )?;
                }
            }
            PhysicalMutationLockTarget::Deleted => {
                recheck_matches = true;
                pending.push((storage_table, doc_id), None, None)?;
            }
        }
        Ok(true)
    })?;
    if recheck_matches {
        referential
            .constraints
            .transactions
            .refresh_explicit_statement_snapshot()?;
    }
    let mut changes = TargetChanges::new(&memory, referential.constraints.cancellation.clone());
    for position in 0..pending.len() {
        let entry = pending.record(position)?;
        let document = match &entry.current {
            Some((table, id)) => referential.constraints.reads.get_document(table, *id)?,
            None => None,
        };
        let current = document
            .as_ref()
            .and(entry.current.as_ref())
            .map(|(table, id)| (table.as_str(), *id));
        changes.push((&entry.original.0, entry.original.1), current, document)?;
    }
    Ok(LockedMergeTargets {
        recheck_matches,
        changes,
    })
}
