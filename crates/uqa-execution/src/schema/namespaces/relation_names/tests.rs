//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::row_locks::{RowLockManager, ScopedRelationLock};
use std::cell::Cell;

struct Session {
    locks: RowLockManager,
    cancellation: uqa_core::CancellationToken,
    refreshed: Cell<bool>,
    fail_refresh: bool,
}

impl Session {
    fn new() -> Self {
        Self {
            locks: RowLockManager::new(),
            cancellation: uqa_core::CancellationToken::new(),
            refreshed: Cell::new(false),
            fail_refresh: false,
        }
    }

    fn available(&self, name: &str) -> bool {
        let key = self.locks.shared_catalog_key(SharedCatalogLock::Name {
            class_id: RELATION_CATALOG_CLASS_ID,
            name,
        });
        let available = self
            .locks
            .try_acquire_relation(
                2,
                key,
                RelationLockMode::AccessExclusive,
                0,
                &uqa_core::CancellationToken::new(),
            )
            .unwrap();
        self.locks.release_session(2);
        available
    }
}

impl SharedObjectLockSession for Session {
    fn acquire_shared_catalog(
        &self,
        target: SharedCatalogLock<'_>,
        mode: RelationLockMode,
    ) -> Result<ScopedRelationLock<'_>, SQLError> {
        self.locks.acquire_scoped_relation(
            1,
            self.locks.shared_catalog_key(target),
            mode,
            (3, 4),
            &self.cancellation,
        )
    }

    fn refresh_shared_catalog(&self) -> Result<(), SQLError> {
        if self.fail_refresh {
            return Err(SQLError::Routine {
                sqlstate: "40001".into(),
                message: "catalog refresh rejected".into(),
            });
        }
        self.refreshed.set(true);
        Ok(())
    }
}

#[test]
fn reservation_detects_a_newly_visible_competitor_and_releases_only_its_candidate() {
    let session = Session::new();
    let retained = RelationIdentity::new("public", "retained");
    reserve_relation_name(&session, &retained, || Ok(false)).unwrap();
    session.refreshed.set(false);
    let target = RelationIdentity::new("public", "target");
    let error =
        reserve_relation_name(&session, &target, || Ok(session.refreshed.get())).unwrap_err();
    assert_eq!(error.sqlstate(), Some("23505"));
    assert!(session.available("public.target"));
    assert!(!session.available("public.retained"));
    assert!(session.available("other.retained"));
    session.locks.release_mark_above(1, 2);
    assert!(session.available("public.retained"));
}

#[test]
fn reservation_preserves_refresh_and_lookup_errors_and_drops_failed_candidates() {
    for refresh in [true, false] {
        let mut session = Session::new();
        session.fail_refresh = refresh;
        let error =
            reserve_relation_name(&session, &RelationIdentity::new("public", "target"), || {
                assert!(!refresh);
                Err(SQLError::Routine {
                    sqlstate: "40001".into(),
                    message: "catalog read rejected".into(),
                })
            })
            .unwrap_err();
        assert_eq!(error.sqlstate(), Some("40001"));
        assert!(session.available("public.target"));
    }
}

#[test]
fn cancelled_reservations_preserve_cancellation_without_reading_the_catalog() {
    let session = Session::new();
    session.cancellation.cancel();
    let error = reserve_relation_name(&session, &RelationIdentity::new("public", "target"), || {
        panic!("cancelled reservation reached catalog lookup")
    })
    .unwrap_err();
    assert_eq!(error.sqlstate(), Some("57014"));
    assert!(!session.refreshed.get());
    assert!(session.available("public.target"));
}
