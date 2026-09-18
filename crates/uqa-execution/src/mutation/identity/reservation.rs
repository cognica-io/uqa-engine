//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Reserve physical row identities before staging mutation images.

use crate::row_locks::{LockAcquire, RowLockAcquisition};
use uqa_core::DocId;
use uqa_sql::SQLError;

/// Keep an unused identity reserved until transaction completion. Candidate reservations must skip conflicting holders: an allocator must never wait for an unrelated INSERT before evaluating its remaining row-locking expressions. The occupancy read follows the grant and includes the latest committed state, since a previous reservation may have committed and been released after the caller's statement snapshot was pinned.
pub fn reserve_document_id(
    mut next_candidate: impl FnMut() -> Result<DocId, SQLError>,
    mut try_reserve: impl FnMut(DocId) -> Result<LockAcquire, SQLError>,
    mut occupied: impl FnMut(DocId) -> Result<bool, SQLError>,
    mut release: impl FnMut(RowLockAcquisition),
) -> Result<DocId, SQLError> {
    loop {
        let candidate = next_candidate()?;
        let LockAcquire::Granted { acquisition, .. } = try_reserve(candidate)? else {
            continue;
        };
        let exists = occupied(candidate);
        if matches!(exists, Ok(false)) {
            return Ok(candidate);
        }
        if let Some(acquisition) = acquisition {
            release(acquisition);
        }
        exists?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::row_locks::{LockRequest, RowLockKey, RowLockManager};
    use proptest::prelude::*;
    use uqa_core::CancellationToken;
    use uqa_sql::ast::{LockStrength, LockWait};

    proptest! {
        #[test]
        fn independent_allocators_reserve_only_distinct_unoccupied_identities(
            states in proptest::collection::vec((any::<bool>(), any::<bool>()), 0..24),
            count in 1usize..8,
            start in 1u64..1000,
        ) {
            let manager = RowLockManager::new();
            let cancel = CancellationToken::new();
            let table = manager.document_identity_key("items");
            let acquire = |session_id, doc_id| {
                manager.acquire(&LockRequest {
                    session_id,
                    key: RowLockKey { table, doc_id },
                    strength: LockStrength::ForUpdate,
                    mark: 0,
                    wait: LockWait::SkipLocked,
                    cancel: &cancel,
                    relation: "items",
                })
            };
            for (index, &(reserved, _)) in states.iter().enumerate() {
                if reserved {
                    acquire(1, start + index as u64).unwrap();
                }
            }
            let expected: Vec<_> = (start..)
                .filter(|id| {
                    !states.get((id - start) as usize)
                        .is_some_and(|&(reserved, occupied)| reserved || occupied)
                })
                .take(count)
                .collect();
            let mut selected = Vec::new();
            for session in 10..10 + count as u64 {
                let mut candidates = start..;
                selected.push(reserve_document_id(
                    || Ok(candidates.next().unwrap()),
                    |id| acquire(session, id),
                    |id| Ok(states.get((id - start) as usize)
                        .is_some_and(|&(_, occupied)| occupied)),
                    |acquisition| manager.rollback_acquisition(acquisition),
                ).unwrap());
            }
            prop_assert_eq!(&selected, &expected);
            for id in start..=*selected.last().unwrap() {
                let retained = selected.contains(&id)
                    || states.get((id - start) as usize)
                        .is_some_and(|&(reserved, _)| reserved);
                prop_assert_eq!(matches!(acquire(100, id).unwrap(), LockAcquire::Skipped), retained);
            }
        }
    }

    #[test]
    fn skips_reserved_and_committed_candidates_without_releasing_the_selected_identity() {
        let manager = RowLockManager::new();
        let cancel = CancellationToken::new();
        let table = manager.document_identity_key("items");
        let acquire = |session_id, doc_id| {
            manager.acquire(&LockRequest {
                session_id,
                key: RowLockKey { table, doc_id },
                strength: LockStrength::ForUpdate,
                mark: 0,
                wait: LockWait::SkipLocked,
                cancel: &cancel,
                relation: "items",
            })
        };
        assert!(matches!(
            acquire(1, 1).unwrap(),
            LockAcquire::Granted { .. }
        ));
        let mut candidates = 1..=3;
        let mut checked = Vec::new();
        let selected = reserve_document_id(
            || Ok(candidates.next().expect("available candidate")),
            |id| acquire(2, id),
            |id| {
                checked.push(id);
                Ok(id == 2)
            },
            |acquisition| manager.rollback_acquisition(acquisition),
        )
        .unwrap();
        assert_eq!(selected, 3);
        assert_eq!(checked, vec![2, 3]);
        assert!(matches!(acquire(3, 1).unwrap(), LockAcquire::Skipped));
        assert!(matches!(
            acquire(3, 2).unwrap(),
            LockAcquire::Granted { .. }
        ));
        assert!(matches!(acquire(3, 3).unwrap(), LockAcquire::Skipped));
        assert!(matches!(
            manager
                .acquire(&LockRequest {
                    session_id: 3,
                    key: RowLockKey {
                        table: manager.table_key("items"),
                        doc_id: 3
                    },
                    strength: LockStrength::ForUpdate,
                    mark: 0,
                    wait: LockWait::SkipLocked,
                    cancel: &cancel,
                    relation: "items",
                })
                .unwrap(),
            LockAcquire::Granted { .. }
        ));
        manager.release_session(2);
        assert!(matches!(
            acquire(3, 3).unwrap(),
            LockAcquire::Granted { .. }
        ));
    }

    #[test]
    fn failed_occupancy_read_releases_the_candidate_reservation() {
        let manager = RowLockManager::new();
        let cancel = CancellationToken::new();
        let table = manager.document_identity_key("items");
        let acquire = |session_id| {
            manager.acquire(&LockRequest {
                session_id,
                key: RowLockKey { table, doc_id: 1 },
                strength: LockStrength::ForUpdate,
                mark: 0,
                wait: LockWait::SkipLocked,
                cancel: &cancel,
                relation: "items",
            })
        };
        let error = reserve_document_id(
            || Ok(1),
            |_| acquire(1),
            |_| Err(SQLError::Internal("injected storage read failure".into())),
            |acquisition| manager.rollback_acquisition(acquisition),
        )
        .unwrap_err();
        assert!(error.to_string().contains("injected storage read failure"));
        assert!(matches!(acquire(2).unwrap(), LockAcquire::Granted { .. }));
    }
}
