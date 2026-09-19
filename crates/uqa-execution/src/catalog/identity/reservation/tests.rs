//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::row_locks::{RowLockManager, ScopedRelationLock};
use std::cell::{Cell, RefCell};
use std::collections::BTreeSet;

struct Session {
    locks: RowLockManager,
    cancellation: uqa_core::CancellationToken,
    occupied: RefCell<BTreeSet<i64>>,
    after_refresh: Cell<Option<i64>>,
    fail_refresh: Cell<bool>,
}

impl Session {
    fn new() -> Self {
        Self {
            locks: RowLockManager::new(),
            cancellation: uqa_core::CancellationToken::new(),
            occupied: RefCell::new(BTreeSet::new()),
            after_refresh: Cell::new(None),
            fail_refresh: Cell::new(false),
        }
    }

    fn available(&self, class_id: u32, oid: u32) -> bool {
        let key = self
            .locks
            .shared_catalog_key(SharedCatalogLock::Object { class_id, oid });
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
        if self.fail_refresh.get() {
            return Err(SQLError::Routine {
                sqlstate: "40001".into(),
                message: "catalog refresh rejected".into(),
            });
        }
        if let Some(oid) = self.after_refresh.take() {
            self.occupied.borrow_mut().insert(oid);
        }
        Ok(())
    }
}

#[test]
fn reservations_skip_existing_and_newly_committed_collisions_and_retain_only_the_final_address() {
    let session = Session::new();
    session.occupied.borrow_mut().insert(20_001);
    session.after_refresh.set(Some(20_002));
    let mut candidates = [20_001, 20_002, 20_003].into_iter();
    let oid = reserve_catalog_oid(
        &session,
        2606,
        "constraint",
        |oid| Ok(session.occupied.borrow().contains(&oid)),
        || Ok(candidates.next().unwrap()),
    )
    .unwrap();
    assert_eq!(oid, 20_003);
    assert!(session.available(2606, 20_001));
    assert!(session.available(2606, 20_002));
    assert!(!session.available(2606, 20_003));
    assert!(session.available(1259, 20_003));
    session.locks.release_mark_above(1, 2);
    assert!(session.available(2606, 20_003));
}

#[test]
fn failed_catalog_refresh_or_lookup_preserves_the_error_and_releases_the_candidate() {
    for refresh in [true, false] {
        let session = Session::new();
        session.fail_refresh.set(refresh);
        let mut calls = 0;
        let error = reserve_catalog_oid(
            &session,
            1259,
            "index",
            |_| {
                calls += 1;
                if calls == 2 {
                    Err(SQLError::Routine {
                        sqlstate: "40001".into(),
                        message: "catalog lookup rejected".into(),
                    })
                } else {
                    Ok(false)
                }
            },
            || Ok(20_001),
        )
        .unwrap_err();
        assert_eq!(error.sqlstate(), Some("40001"));
        assert!(session.available(1259, 20_001));
    }
}

#[test]
fn invalid_catalog_allocations_fail_before_catalog_lookup_or_lock_acquisition() {
    let session = Session::new();
    for oid in [-1, 0, 16_383, i64::from(u32::MAX) + 1] {
        assert!(reserve_catalog_oid(
            &session,
            2606,
            "constraint",
            |_| panic!("invalid OID reached catalog lookup"),
            || Ok(oid),
        )
        .unwrap_err()
        .to_string()
        .contains("invalid constraint OID allocation"));
    }
}

#[test]
fn cancelled_reservations_do_not_reclassify_or_retry_the_lock_error() {
    let session = Session::new();
    session.cancellation.cancel();
    let mut attempts = 0;
    let error = reserve_catalog_oid(
        &session,
        2606,
        "constraint",
        |_| Ok(false),
        || {
            attempts += 1;
            Ok(20_001)
        },
    )
    .unwrap_err();
    assert_eq!(error.sqlstate(), Some("57014"));
    assert_eq!(attempts, 1);
    assert!(session.available(2606, 20_001));
}
