//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::row_locks::RowLockManager;
use std::cell::Cell;
use uqa_core::CancellationToken;

struct Session {
    manager: RowLockManager,
    cancellation: CancellationToken,
    definition: Cell<u32>,
}

impl Session {
    fn new() -> Self {
        Self {
            manager: RowLockManager::new(),
            cancellation: CancellationToken::new(),
            definition: Cell::new(1),
        }
    }
    fn resolve(&self) -> Result<Option<RelationBinding<u32>>, SQLError> {
        Ok(Some(RelationBinding {
            name: "public.v".into(),
            object_id: Some([1; 16]),
            value: self.definition.get(),
        }))
    }
    fn peer_acquires(&self, mode: RelationLockMode) -> bool {
        self.manager
            .try_acquire_relation(
                2,
                self.manager.table_key("public.v"),
                mode,
                0,
                &self.cancellation,
            )
            .unwrap()
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
        self.definition.set(2);
        Ok(())
    }
}

#[test]
fn stable_identity_returns_the_new_definition_after_waiting() {
    let session = Session::new();
    let result = bind_relation(
        &session,
        RelationLockMode::AccessExclusive,
        false,
        || session.resolve(),
        |_| Ok(()),
    )
    .unwrap()
    .unwrap();
    assert_eq!(result.value, 2);
    assert!(!session.peer_acquires(RelationLockMode::AccessShare));
    session.manager.release_session(1);
    assert!(session.peer_acquires(RelationLockMode::AccessExclusive));
}

#[test]
fn changed_relation_kind_replaces_the_provisional_lock_mode() {
    let session = Session::new();
    let result = bind_relation_with_mode(
        &session,
        |binding: &RelationBinding<u32>| {
            if binding.value == 1 {
                RelationLockMode::ShareUpdateExclusive
            } else {
                RelationLockMode::AccessExclusive
            }
        },
        false,
        || session.resolve(),
        |_| Ok(()),
    )
    .unwrap()
    .unwrap();
    assert_eq!(result.value, 2);
    assert!(!session.peer_acquires(RelationLockMode::AccessShare));
    session.manager.release_session(1);
    assert!(session.peer_acquires(RelationLockMode::AccessExclusive));
}

#[test]
fn revoked_authority_discards_only_the_provisional_lock_upgrade() {
    let session = Session::new();
    session
        .acquire("public.v", RelationLockMode::AccessShare, false)
        .unwrap()
        .unwrap()
        .retain();
    session.definition.set(1);
    let result = bind_relation(
        &session,
        RelationLockMode::AccessExclusive,
        false,
        || session.resolve(),
        |binding| {
            if binding.value == 1 {
                Ok(())
            } else {
                Err(SQLError::Routine {
                    sqlstate: "42501".into(),
                    message: "ownership changed".into(),
                })
            }
        },
    );
    assert_eq!(result.err().unwrap().sqlstate(), Some("42501"));
    assert!(session.peer_acquires(RelationLockMode::RowExclusive));
    assert!(!session.peer_acquires(RelationLockMode::AccessExclusive));
}
