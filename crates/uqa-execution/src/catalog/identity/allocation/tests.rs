//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::{CatalogReadView, RelationNameResolution};
use crate::row_locks::{
    shared_objects::SharedCatalogLock, RelationLockMode, RowLockManager, ScopedRelationLock,
};
use parking_lot::{Mutex, RwLock};
use uqa_sql::{ast::ConstraintCatalogIdentity, SQLError};

struct Session {
    current: RwLock<CatalogReadView>,
    refresh: Mutex<Option<CatalogReadView>>,
    failure: Option<&'static str>,
    locks: RowLockManager,
    cancellation: uqa_core::CancellationToken,
}

impl Session {
    fn new() -> Self {
        Self {
            current: RwLock::new(crate::catalog::test_support::empty_catalog()),
            refresh: Mutex::new(None),
            failure: None,
            locks: RowLockManager::new(),
            cancellation: uqa_core::CancellationToken::new(),
        }
    }

    fn allocator(&self) -> ReservedCatalogIdentityAllocator<'_> {
        CatalogIdentityReservationContext {
            catalog: self,
            session: self,
            locks: self,
        }
        .allocator(crate::catalog::identity::allocate_catalog_object_id)
    }

    fn available(&self, oid: i64) -> bool {
        let key = self.locks.shared_catalog_key(SharedCatalogLock::Object {
            class_id: 2606,
            oid: u32::try_from(oid).unwrap(),
        });
        let available = self
            .locks
            .try_acquire_relation(
                2,
                key,
                RelationLockMode::AccessExclusive,
                0,
                &self.cancellation,
            )
            .unwrap();
        self.locks.release_session(2);
        available
    }
}

impl CatalogSnapshotSource for Session {
    fn catalog_snapshot(&self) -> CatalogReadView {
        crate::catalog::test_support::empty_catalog()
    }
    fn refreshed_catalog_snapshot(&self) -> Result<CatalogReadView, SQLError> {
        Ok(self.catalog_snapshot())
    }
    fn current_catalog_snapshot(&self) -> CatalogReadView {
        self.current.read().clone()
    }
}

impl CatalogSession for Session {
    fn current_role(&self) -> uqa_sql::catalog::roles::RoleReference {
        "restricted".into()
    }
    fn temporary_schema_name(&self) -> String {
        "pg_temp_1".into()
    }
    fn relation_name_resolution(&self) -> RelationNameResolution {
        RelationNameResolution {
            search_path: vec!["public".into()],
            temporary_schema: "pg_temp_1".into(),
            temporary_namespace_allocated: false,
            current_user: "restricted".into(),
            lookup_mode: crate::catalog::RelationLookupMode::Dynamic,
        }
    }
    fn show_variable(&self, _: &str) -> Result<String, SQLError> {
        unreachable!()
    }
    fn runtime_parameter_source(&self, _: &str) -> &'static str {
        unreachable!()
    }
    fn prepared_statements(&self) -> Vec<uqa_sql::catalog::session::PreparedStatementMetadata> {
        Vec::new()
    }
    fn cursors(&self) -> Vec<uqa_sql::catalog::session::CursorMetadata> {
        Vec::new()
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
        if let Some(code) = self.failure {
            return Err(SQLError::Routine {
                sqlstate: code.into(),
                message: "refresh failed".into(),
            });
        }
        if let Some(refreshed) = self.refresh.lock().take() {
            *self.current.write() = refreshed;
        }
        Ok(())
    }
}

fn occupied(oid: i64) -> CatalogReadView {
    let mut snapshot = crate::catalog::test_support::empty_catalog()
        .snapshot()
        .clone();
    let uqa_sql::Statement::CreateTable(table) =
        uqa_sql::compile("CREATE TABLE peer(v int NOT NULL)")
            .unwrap()
            .remove(0)
    else {
        panic!("table declaration")
    };
    let mut columns = table.columns;
    columns[0].not_null_identity = Some(ConstraintCatalogIdentity {
        object_id: [91; 16],
        oid,
    });
    snapshot.definitions.foreign_tables = std::collections::BTreeMap::from([(
        uqa_core::RelationIdentity::new("hidden_schema", "peer"),
        crate::catalog::foreign::StoredForeignTable {
            name: "hidden_schema.peer".into(),
            object_id: [90; 16],
            server_name: "memory".into(),
            columns,
            checks: Vec::new(),
            options: std::collections::BTreeMap::default(),
        },
    )])
    .into();
    CatalogReadView::new(snapshot)
}

#[test]
fn allocation_uses_current_authority_independent_metadata_before_and_after_refresh() {
    let object = [12; 16];
    let candidate = uqa_sql::catalog::oids::stable_object_oid("constraint", &object);
    assert!(candidate >= 16_384);
    for after_refresh in [false, true] {
        let session = Session::new();
        if after_refresh {
            *session.refresh.lock() = Some(occupied(candidate));
        } else {
            *session.current.write() = occupied(candidate);
        }
        let oid = session
            .allocator()
            .allocate_catalog_oid(CatalogOidClass::Constraint, &object)
            .unwrap();
        assert_ne!(oid, candidate);
        assert!(session.available(candidate));
        assert!(!session.available(oid));
        session.locks.release_mark_above(1, 2);
        assert!(session.available(oid));
    }
}

#[test]
fn allocation_excludes_preexisting_and_new_addresses_in_the_same_unpublished_candidate() {
    let session = Session::new();
    let mut allocator = session.allocator();
    let object = [12; 16];
    let existing = uqa_sql::catalog::oids::stable_object_oid("constraint", &object);
    allocator
        .include_catalog_identity(
            &uqa_core::RelationIdentity::new("public", "target"),
            CatalogOidClass::Constraint,
            ConstraintCatalogIdentity {
                object_id: [1; 16],
                oid: existing,
            },
        )
        .unwrap();
    let first = allocator
        .allocate_catalog_oid(CatalogOidClass::Constraint, &object)
        .unwrap();
    let second = allocator
        .allocate_catalog_oid(CatalogOidClass::Constraint, &object)
        .unwrap();
    assert_ne!(existing, first);
    assert_ne!(existing, second);
    assert_ne!(first, second);
    assert!(!session.available(existing));
    assert!(!session.available(first));
    assert!(!session.available(second));
}

#[test]
fn allocation_keeps_sqlstate_through_the_storage_error_boundary_and_releases_failed_candidates() {
    for code in ["40001", "57014"] {
        let mut session = Session::new();
        session.failure = Some(code);
        let failure = session
            .allocator()
            .allocate_catalog_oid(CatalogOidClass::Constraint, &[12; 16])
            .unwrap_err();
        let failure = uqa_storage::StorageBackendError::backend("constraint identity", failure);
        assert_eq!(
            uqa_sql::catalog::errors::storage_error("materialize", &failure).sqlstate(),
            Some(code)
        );
        let oid = uqa_sql::catalog::oids::stable_object_oid("constraint", &[12; 16]);
        assert!(session.available(oid));
    }
}

#[test]
fn supplied_identities_require_the_existing_relation_and_both_identity_components() {
    let session = Session::new();
    let identity = ConstraintCatalogIdentity {
        object_id: [91; 16],
        oid: 50001,
    };
    *session.current.write() = occupied(identity.oid);
    let owner = uqa_core::RelationIdentity::new("hidden_schema", "peer");
    session
        .allocator()
        .include_catalog_identity(&owner, CatalogOidClass::Constraint, identity)
        .unwrap();
    assert!(session.available(identity.oid));
    for (relation, supplied) in [
        (
            uqa_core::RelationIdentity::new("public", "target"),
            identity,
        ),
        (
            owner.clone(),
            ConstraintCatalogIdentity {
                object_id: [92; 16],
                ..identity
            },
        ),
        (
            owner,
            ConstraintCatalogIdentity {
                oid: identity.oid + 1,
                ..identity
            },
        ),
    ] {
        let failure = session
            .allocator()
            .include_catalog_identity(&relation, CatalogOidClass::Constraint, supplied)
            .unwrap_err();
        assert!(
            failure
                .to_string()
                .contains("conflicts with an existing row"),
            "{failure}"
        );
        assert!(session.available(supplied.oid));
    }
}

#[test]
fn supplied_new_addresses_reserve_through_savepoints_and_reject_post_wait_collisions() {
    for after_refresh in [false, true] {
        let session = Session::new();
        let identity = ConstraintCatalogIdentity {
            object_id: [12; 16],
            oid: 50001,
        };
        if after_refresh {
            *session.refresh.lock() = Some(occupied(identity.oid));
        }
        let result = session.allocator().include_catalog_identity(
            &uqa_core::RelationIdentity::new("public", "target"),
            CatalogOidClass::Constraint,
            identity,
        );
        assert_eq!(result.is_err(), after_refresh);
        assert_eq!(session.available(identity.oid), after_refresh);
        session.locks.release_mark_above(1, 2);
        assert!(session.available(identity.oid));
    }
}

#[test]
fn supplied_identity_reservation_releases_failures_and_preserves_sqlstate() {
    for code in ["40001", "57014"] {
        let mut session = Session::new();
        session.failure = Some(code);
        let failure = session
            .allocator()
            .include_catalog_identity(
                &uqa_core::RelationIdentity::new("public", "target"),
                CatalogOidClass::Constraint,
                ConstraintCatalogIdentity {
                    object_id: [12; 16],
                    oid: 50001,
                },
            )
            .unwrap_err();
        let failure = uqa_storage::StorageBackendError::backend("constraint identity", failure);
        assert_eq!(
            uqa_sql::catalog::errors::storage_error("materialize", &failure).sqlstate(),
            Some(code)
        );
        assert!(session.available(50001));
    }
}
