//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The paired targets a MERGE locks, followed to the row versions a committed update chain moved them to.

use super::{analysis::merge_target_lock_strength, model::MergeTargetIdentity};
use crate::mutation::{
    candidate::PhysicalMutationLockTarget, locking::lock_physical_mutation_target,
    referential::ReferentialContext,
};
use std::collections::{BTreeMap, BTreeSet};
use uqa_sql::{plan::MergePlan, SQLError};
use uqa_storage::document_store::Document;

/// The paired targets of a MERGE once locked.
pub(super) struct LockedMergeTargets {
    /// Whether a wait or a successor makes the statement requalify its matches.
    pub(super) recheck_matches: bool,
    /// The targets a committed transaction deleted while the statement waited.
    pub(super) deleted: BTreeSet<MergeTargetIdentity>,
    /// The targets to act on in their current version, with the identity each holds now.
    pub(super) refreshed: BTreeMap<MergeTargetIdentity, (MergeTargetIdentity, Document)>,
}

/// Lock the paired targets of a MERGE. A target may have been moved to a successor identity by a primary-key rewrite that another transaction committed while the statement waited; `PostgreSQL` 18 follows the update chain, so the pairing is redirected to the successor before the actions run.
pub(super) fn lock_merge_targets<S: Clone + 'static>(
    referential: &ReferentialContext<'_, S>,
    stmt: &MergePlan,
    target_qualifier: &str,
    targets: BTreeSet<MergeTargetIdentity>,
) -> Result<LockedMergeTargets, SQLError> {
    let mut recheck_matches = false;
    let mut successors: BTreeMap<MergeTargetIdentity, MergeTargetIdentity> = BTreeMap::new();
    let mut rechecked_target_ids = BTreeSet::new();
    let mut deleted = BTreeSet::new();
    for (storage_table, doc_id) in targets {
        let original_identity = (storage_table.clone(), doc_id);
        let target = lock_physical_mutation_target(
            referential.locking.session,
            &storage_table,
            target_qualifier,
            doc_id,
            merge_target_lock_strength(referential.locking.catalog, stmt, &storage_table),
        )?;
        match target {
            PhysicalMutationLockTarget::Present { identity, recheck } => {
                recheck_matches |= recheck;
                let locked_identity = (identity.table, identity.doc_id);
                if recheck || locked_identity != original_identity {
                    rechecked_target_ids.insert(original_identity.clone());
                }
                if locked_identity != original_identity {
                    successors.insert(original_identity, locked_identity);
                }
            }
            PhysicalMutationLockTarget::Deleted => {
                recheck_matches = true;
                deleted.insert(original_identity);
            }
        }
    }
    if recheck_matches {
        referential
            .constraints
            .transactions
            .refresh_explicit_statement_snapshot()?;
    }
    let mut refreshed = BTreeMap::new();
    for original_identity in rechecked_target_ids {
        let locked_identity = successors
            .get(&original_identity)
            .cloned()
            .unwrap_or_else(|| original_identity.clone());
        if let Some(document) = referential
            .constraints
            .reads
            .get_document(&locked_identity.0, locked_identity.1)?
        {
            refreshed.insert(original_identity, (locked_identity, document));
        } else {
            deleted.insert(original_identity);
        }
    }
    Ok(LockedMergeTargets {
        recheck_matches,
        deleted,
        refreshed,
    })
}
