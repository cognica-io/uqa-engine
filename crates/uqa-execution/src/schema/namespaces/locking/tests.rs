//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::row_locks::{
    PhysicalRowChangeTarget, RowChangeTarget, RowLockManager, ScopedRelationLock,
};
use std::{cell::RefCell, collections::VecDeque, sync::Arc};
use uqa_core::CancellationToken;

struct Session {
    manager: Arc<RowLockManager>,
    cancellation: CancellationToken,
    current: RefCell<Option<BoundSchemaSecurity>>,
    refreshes: RefCell<VecDeque<Option<BoundSchemaSecurity>>>,
}

impl Session {
    fn new(current: BoundSchemaSecurity, next: Option<BoundSchemaSecurity>) -> Self {
        Self {
            manager: Arc::new(RowLockManager::new()),
            cancellation: CancellationToken::new(),
            current: RefCell::new(Some(current)),
            refreshes: RefCell::new(VecDeque::from([next])),
        }
    }
    fn context(&self) -> SchemaLockContext<'_> {
        SchemaLockContext {
            objects: self,
            relations: self,
            rows: self,
            catalog: self,
        }
    }
    fn object_key(&self, oid: u32) -> u64 {
        self.manager.shared_catalog_key(SharedCatalogLock::Object {
            class_id: SCHEMA_CATALOG_CLASS_ID,
            oid,
        })
    }
    fn peer_acquires(&self, oid: u32) -> bool {
        self.manager
            .try_acquire_relation(
                2,
                self.object_key(oid),
                RelationLockMode::AccessExclusive,
                0,
                &self.cancellation,
            )
            .unwrap()
    }
}
impl SchemaSecurityCatalog for Session {
    fn schema_security(&self, _: &str) -> Option<BoundSchemaSecurity> {
        self.current.borrow().clone()
    }
}
impl SharedObjectLockSession for Session {
    fn acquire_shared_catalog(
        &self,
        target: SharedCatalogLock<'_>,
        mode: RelationLockMode,
    ) -> Result<ScopedRelationLock<'_>, SQLError> {
        self.manager.acquire_scoped_relation(
            1,
            self.manager.shared_catalog_key(target),
            mode,
            (0, 1),
            &self.cancellation,
        )
    }
    fn refresh_shared_catalog(&self) -> Result<(), SQLError> {
        if let Some(next) = self.refreshes.borrow_mut().pop_front() {
            *self.current.borrow_mut() = next;
        }
        Ok(())
    }
}
impl RelationLockSession for Session {
    fn acquire(
        &self,
        name: &str,
        mode: RelationLockMode,
        _: bool,
    ) -> Result<Option<ScopedRelationLock<'_>>, SQLError> {
        self.manager
            .acquire_scoped_relation(
                1,
                self.manager.table_key(name),
                mode,
                (0, 1),
                &self.cancellation,
            )
            .map(Some)
    }
    fn refresh_after_wait(&self) -> Result<(), SQLError> {
        self.refresh_shared_catalog()
    }
}
impl RowLockSession for Session {
    fn lock_manager(&self) -> Arc<RowLockManager> {
        self.manager.clone()
    }
    fn lock_row(
        &self,
        table: &str,
        _: u64,
        strength: LockStrength,
        wait: LockWait,
        _: &str,
    ) -> Result<LockAcquire, SQLError> {
        assert_eq!(table, "pg_catalog.pg_namespace");
        assert_eq!(strength, LockStrength::ForNoKeyUpdate);
        assert_eq!(wait, LockWait::Block);
        Ok(LockAcquire::Granted {
            waited: true,
            foreign_waited: false,
            acquisition: None,
        })
    }
    fn uses_fixed_snapshot(&self) -> bool {
        true
    }
    fn committed_row_successor(&self, _: &str, _: u64) -> Result<RowChangeTarget, SQLError> {
        unreachable!()
    }
    fn committed_physical_row_successor(
        &self,
        _: &str,
        _: u64,
    ) -> Result<PhysicalRowChangeTarget, SQLError> {
        unreachable!()
    }
    fn table_for_lock_hash(&self, _: u64) -> Result<String, SQLError> {
        unreachable!()
    }
}

#[test]
fn lifetime_rebinding_releases_the_old_lock_and_retains_the_replacement() {
    let before = BoundSchemaSecurity::bootstrap("s");
    let old_oid = before.tuple.unwrap().oid as u32;
    let mut next = before.clone();
    next.tuple = Some(SchemaTupleIdentity {
        oid: 42_001,
        object_id: [8; 16],
        revision: [9; 16],
    });
    let session = Session::new(before, Some(next.clone()));
    assert_eq!(
        session
            .context()
            .bind_lifetime("s", RelationLockMode::AccessShare)
            .unwrap(),
        Some(next)
    );
    assert!(session.peer_acquires(old_oid));
    assert!(!session.peer_acquires(42_001));
    session.manager.release_mark_above(1, 0);
    assert!(!session.peer_acquires(42_001));
}

#[test]
fn deleted_lifetime_returns_missing_and_releases_its_provisional_lock() {
    let before = BoundSchemaSecurity::bootstrap("s");
    let old_oid = before.tuple.unwrap().oid as u32;
    let session = Session::new(before, None);
    assert!(session
        .context()
        .bind_lifetime("s", RelationLockMode::AccessShare)
        .unwrap()
        .is_none());
    assert!(session.peer_acquires(old_oid));
}

#[test]
fn tuple_checks_distinguish_updates_deletion_and_oid_reuse_from_abort() {
    let before = BoundSchemaSecurity::bootstrap("s");
    for action in ["abort", "updated", "deleted", "reused"] {
        let mut next = before.clone();
        match action {
            "updated" => next.tuple.as_mut().unwrap().revision = [8; 16],
            "reused" => next.tuple.as_mut().unwrap().object_id = [9; 16],
            _ => {}
        }
        let session = Session::new(before.clone(), (action != "deleted").then_some(next));
        let result = session.context().replace("s", before.tuple.unwrap());
        if action == "abort" {
            result.unwrap();
        } else {
            let error = result.unwrap_err();
            assert_eq!(error.sqlstate(), Some("XX000"));
            assert!(error.to_string().contains(if action == "updated" {
                "concurrently updated"
            } else {
                "concurrently deleted"
            }));
        }
    }
}
