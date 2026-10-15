//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::{
    domain::DomainRegistry, foreign::StoredForeignTable, test_support, CatalogReadSnapshot,
};
use crate::row_locks::{RowLockManager, ScopedRelationLock};
use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    sync::Arc,
};
use uqa_sql::catalog::events::TriggerCatalog;

const OWNER: [u8; 16] = [1; 16];

fn columns(occupied: bool) -> Vec<ColumnDef> {
    let sql = if occupied {
        "CREATE TABLE t(v int CONSTRAINT claimed CHECK(v>0))"
    } else {
        "CREATE TABLE t(v int)"
    };
    let uqa_sql::Statement::CreateTable(table) = uqa_sql::compile(sql).unwrap().remove(0) else {
        panic!("table declaration")
    };
    table.columns
}

fn snapshot(occupied: bool, object_id: [u8; 16]) -> CatalogReadSnapshot {
    let mut snapshot = test_support::empty_catalog().snapshot().clone();
    snapshot.tables.insert(
        RelationIdentity::new("public", "t"),
        test_support::table_snapshot(object_id, columns(occupied), TableConstraintSet::default()),
    );
    snapshot
}

struct Session {
    current: RefCell<CatalogReadSnapshot>,
    after_refresh: RefCell<Option<CatalogReadSnapshot>>,
    fail_refresh: Cell<bool>,
    locks: RowLockManager,
    cancellation: uqa_core::CancellationToken,
}

impl Session {
    fn new(after_refresh: CatalogReadSnapshot) -> Self {
        Self {
            current: RefCell::new(snapshot(false, OWNER)),
            after_refresh: RefCell::new(Some(after_refresh)),
            fail_refresh: Cell::new(false),
            locks: RowLockManager::new(),
            cancellation: uqa_core::CancellationToken::new(),
        }
    }
    fn context(&self) -> ConstraintNameContext<'_> {
        ConstraintNameContext {
            catalog: self,
            locks: self,
        }
    }
    fn available(&self) -> bool {
        let key = self
            .locks
            .shared_catalog_key(SharedCatalogLock::MemberName {
                class_id: CatalogOidClass::Constraint.class_id(),
                owner_class_id: CatalogOidClass::Relation.class_id(),
                owner_object_id: OWNER,
                name: "claimed",
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

impl ConstraintNameCatalog for Session {
    fn constraint_table(&self, relation: &RelationIdentity) -> Option<ConstraintTableMetadata> {
        self.current.borrow().constraint_table(relation)
    }
    fn constraint_owner(
        &self,
        object_id: [u8; 16],
    ) -> Option<(RelationIdentity, ConstraintTableMetadata)> {
        self.current.borrow().constraint_owner(object_id)
    }
    fn constraint_schema_tables(&self, schema: &str) -> Vec<ConstraintTableMetadata> {
        self.current.borrow().constraint_schema_tables(schema)
    }
    fn constraint_foreign_tables(&self) -> Arc<BTreeMap<RelationIdentity, StoredForeignTable>> {
        self.current.borrow().constraint_foreign_tables()
    }
    fn constraint_domains(&self) -> Arc<DomainRegistry> {
        self.current.borrow().constraint_domains()
    }
    fn constraint_triggers(&self) -> Arc<TriggerCatalog> {
        self.current.borrow().constraint_triggers()
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
                message: "refresh failed".into(),
            });
        }
        if let Some(snapshot) = self.after_refresh.borrow_mut().take() {
            *self.current.borrow_mut() = snapshot;
        }
        Ok(())
    }
    fn next_catalog_oid(&self) -> Result<u32, SQLError> {
        unreachable!("name reservation does not allocate OIDs")
    }
}

#[test]
fn reservations_recheck_current_owners_and_release_failed_locks() {
    let session = Session::new(snapshot(true, OWNER));
    assert!(
        matches!(session.context().reserve(OWNER, "claimed"), Err(SQLError::Routine { sqlstate, .. }) if sqlstate == "23505")
    );
    assert!(session.available());

    let session = Session::new(snapshot(false, [2; 16]));
    assert!(
        matches!(session.context().reserve(OWNER, "claimed"), Err(SQLError::Internal(message)) if message == "constraint owning relation disappeared")
    );
    assert!(session.available());

    let session = Session::new(snapshot(false, OWNER));
    session.fail_refresh.set(true);
    assert!(
        matches!(session.context().reserve(OWNER, "claimed"), Err(SQLError::Routine { sqlstate, .. }) if sqlstate == "40001")
    );
    assert!(session.available());
    session.fail_refresh.set(false);
    session.context().reserve(OWNER, "claimed").unwrap();
    assert!(!session.available());
    session.locks.release_session(1);
    assert!(session.available());
}

#[test]
fn reservation_refresh_preserves_the_current_owned_index_name() {
    let key: uqa_sql::ast::TableKeyConstraint = serde_json::from_value(serde_json::json!({
        "name": "original", "columns": ["v"], "kind": "Unique",
        "catalog_identity": {"object_id": vec![3; 16], "oid": 16384}
    }))
    .unwrap();
    let relation = RelationIdentity::new("public", "t");
    let mut initial = snapshot(false, OWNER);
    initial.tables.get_mut(&relation).unwrap().keys = Arc::new(vec![key.clone()]);
    let mut refreshed = initial.clone();
    let mut renamed = key.clone();
    renamed.name = Some("renamed".into());
    refreshed.tables.get_mut(&relation).unwrap().keys = Arc::new(vec![renamed.clone()]);
    let session = Session::new(refreshed);
    *session.current.borrow_mut() = initial;
    let mut candidate = TableConstraintSet {
        key_constraints: vec![key],
        ..Default::default()
    };
    session
        .context()
        .reserve_changes(OWNER, &columns(true), &mut candidate)
        .unwrap();
    assert_eq!(candidate.key_constraints, [renamed]);
    assert!(!session.available());
}
