//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Row-change normalization and successor resolution.

use super::{
    resolve_local_physical_change_target, CommittedRowChange, HashSet,
    LocalPhysicalRowChangeTarget, LockGrant, LockStrength, LockTable, PendingRowChange,
    PendingRowChangeKind, RowChangeTarget, RowLockKey,
};

pub(super) fn normalize_pending_row_changes(
    changes: impl IntoIterator<Item = PendingRowChange>,
) -> Vec<PendingRowChange> {
    let changes = changes.into_iter().collect::<Vec<_>>();
    let mut skip = vec![false; changes.len()];
    for (rewrite_index, rewrite) in changes.iter().enumerate() {
        let PendingRowChangeKind::Rewrite(successor) = rewrite.kind else {
            continue;
        };
        let Some(delete_index) = (0..rewrite_index).rev().find(|index| {
            !skip[*index]
                && changes[*index].key == rewrite.key
                && matches!(changes[*index].kind, PendingRowChangeKind::Delete)
        }) else {
            continue;
        };
        let Some(insert_index) = (delete_index + 1..rewrite_index).rev().find(|index| {
            !skip[*index]
                && changes[*index].key == successor
                && matches!(changes[*index].kind, PendingRowChangeKind::Insert)
        }) else {
            continue;
        };
        skip[delete_index] = true;
        skip[insert_index] = true;
        for index in insert_index + 1..rewrite_index {
            if changes[index].key == successor
                && matches!(changes[index].kind, PendingRowChangeKind::Update)
            {
                skip[index] = true;
            }
        }
    }

    let mut created = HashSet::new();
    let mut normalized = Vec::new();
    for (index, change) in changes.into_iter().enumerate() {
        if skip[index] {
            continue;
        }
        match change.kind {
            PendingRowChangeKind::Insert => {
                created.insert(change.key);
            }
            PendingRowChangeKind::Update if created.contains(&change.key) => {}
            PendingRowChangeKind::Delete if created.remove(&change.key) => {}
            PendingRowChangeKind::Rewrite(successor) if created.remove(&change.key) => {
                created.insert(successor);
            }
            PendingRowChangeKind::Update
            | PendingRowChangeKind::Delete
            | PendingRowChangeKind::Rewrite(_) => normalized.push(change),
        }
    }
    normalized
}

pub(super) fn resolve_local_change_target(
    changes: &[CommittedRowChange],
    key: RowLockKey,
    baseline: u64,
    wanted: LockStrength,
) -> RowChangeTarget {
    match resolve_local_physical_change_target(changes, key, baseline, wanted) {
        LocalPhysicalRowChangeTarget::Unchanged => RowChangeTarget::Unchanged,
        LocalPhysicalRowChangeTarget::Present(target) if target.table == key.table => {
            RowChangeTarget::Present(target.doc_id)
        }
        // Callers of the legacy document-id-only API cannot safely follow a tuple into another physical relation. Treat it as absent instead of applying the successor id to an unrelated row in the source relation.
        LocalPhysicalRowChangeTarget::Present(_) | LocalPhysicalRowChangeTarget::Deleted => {
            RowChangeTarget::Deleted
        }
    }
}

pub(super) fn epoch_is_after(candidate: u64, baseline: u64) -> bool {
    let distance = candidate.wrapping_sub(baseline);
    distance != 0 && distance <= u64::MAX / 2
}

pub(super) fn mutation_strength(
    state: &LockTable,
    session_id: u64,
    key: RowLockKey,
) -> LockStrength {
    state
        .rows
        .get(&key)
        .and_then(|grants| grants.iter().find(|grant| grant.session_id == session_id))
        .map(LockGrant::effective_strength)
        .filter(|strength| {
            matches!(
                strength,
                LockStrength::ForNoKeyUpdate | LockStrength::ForUpdate
            )
        })
        .unwrap_or(LockStrength::ForUpdate)
}

pub(super) fn row_has_waiter(state: &LockTable, key: RowLockKey) -> bool {
    state
        .waiting
        .values()
        .any(|requests| requests.contains_key(&key))
}

pub(super) fn remove_inactive_versions(state: &mut LockTable) {
    if state.active_change_observers != 0 {
        return;
    }
    let rows = &state.rows;
    let waiting = &state.waiting;
    state.changes.retain(|change| {
        rows.contains_key(&change.key)
            || waiting
                .values()
                .any(|requests| requests.contains_key(&change.key))
    });
}
