//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::row_locks::{RowLockManager, ScopedRelationLock};
use std::cell::{Cell, RefCell};
use uqa_core::{catalog_identity::CatalogObjectIdentity, CancellationToken, RelationIdentity};
use uqa_sql::{
    ast::{ColumnType, DropKind},
    catalog::{
        index::{IndexCatalogIdentity, IndexDefinition},
        resolution::RelationResolution,
    },
};
use uqa_storage::StorageBackendResult;

struct Snapshot {
    row: Option<CatalogIndexRow>,
    kind: &'static str,
    owned: bool,
    allowed: bool,
}

fn snapshot(id: u8, table: &str) -> Snapshot {
    Snapshot {
        row: Some(CatalogIndexRow {
            relation: RelationIdentity::new("public", "idx"),
            table_name: table.into(),
            index_type: "btree".into(),
            columns_json: "[]".into(),
            parameters_json: "{}".into(),
            definition_json: Some(
                serde_json::to_string(&IndexDefinition {
                    catalog: Some(IndexCatalogIdentity {
                        identity: CatalogObjectIdentity {
                            object_id: [id; 16],
                            oid: 17000 + i64::from(id),
                        },
                        table_object_id: [9; 16],
                        physical_key: format!("physical:{id}"),
                    }),
                    ..IndexDefinition::default()
                })
                .unwrap(),
            ),
        }),
        kind: "index",
        owned: false,
        allowed: true,
    }
}

struct Session {
    current: RefCell<Snapshot>,
    replacement: RefCell<Option<Snapshot>>,
    refreshes: Cell<usize>,
    failure: Option<&'static str>,
    locks: RowLockManager,
    cancellation: CancellationToken,
}

impl Session {
    fn new(replacement: Snapshot) -> Self {
        Self {
            current: RefCell::new(snapshot(1, "public.t")),
            replacement: RefCell::new(Some(replacement)),
            refreshes: Cell::new(0),
            failure: None,
            locks: RowLockManager::new(),
            cancellation: CancellationToken::new(),
        }
    }

    fn bind(
        &self,
        optional: bool,
        notices: &mut Vec<String>,
    ) -> Result<Vec<CatalogIndexRow>, SQLError> {
        bind_drop_targets(
            self,
            self,
            self,
            &DropStmt {
                kind: DropKind::Index,
                names: vec!["idx".into()],
                if_exists: optional,
                cascade: false,
            },
            &mut |notice| notices.push(notice.into()),
        )
    }

    fn peer_acquires(&self, table: &str, mode: RelationLockMode) -> bool {
        let acquired = self
            .locks
            .try_acquire_relation(2, self.locks.table_key(table), mode, 0, &self.cancellation)
            .unwrap();
        self.locks.release_session(2);
        acquired
    }
}

impl IndexRemovalCatalog for Session {
    fn resolve_relation_kind(&self, _: &str) -> Result<RelationResolution, SQLError> {
        let current = self.current.borrow();
        Ok(current
            .row
            .as_ref()
            .map_or(RelationResolution::MissingRelation, |row| {
                RelationResolution::Found(row.relation.qualified_name(), current.kind)
            }))
    }
    fn bound_catalog_index(&self, _: &str) -> StorageBackendResult<Option<CatalogIndexRow>> {
        Ok(self.current.borrow().row.clone())
    }
    fn has_constraint_index(&self, _: &RelationIdentity) -> bool {
        self.current.borrow().owned
    }
    fn list_catalog_indexes(&self) -> StorageBackendResult<Vec<CatalogIndexRow>> {
        unreachable!()
    }
    fn column_type(&self, _: &str, _: &str) -> StorageBackendResult<Option<ColumnType>> {
        unreachable!()
    }
}

impl IndexRemovalPrivileges for Session {
    fn ensure_drop_authority(&self, _: &CatalogIndexRow) -> Result<(), SQLError> {
        if self.current.borrow().allowed {
            Ok(())
        } else {
            Err(SQLError::Routine {
                sqlstate: "42501".into(),
                message: "index owner changed".into(),
            })
        }
    }
}

impl RelationLockSession for Session {
    fn acquire(
        &self,
        name: &str,
        mode: RelationLockMode,
        _: bool,
    ) -> Result<Option<ScopedRelationLock<'_>>, SQLError> {
        self.locks
            .acquire_scoped_relation(
                1,
                self.locks.table_key(name),
                mode,
                (0, 1),
                &self.cancellation,
            )
            .map(Some)
    }
    fn refresh_after_wait(&self) -> Result<(), SQLError> {
        self.refreshes.set(self.refreshes.get() + 1);
        if let Some(code) = self.failure {
            return Err(SQLError::Routine {
                sqlstate: code.into(),
                message: "catalog refresh failed".into(),
            });
        }
        if let Some(replacement) = self.replacement.borrow_mut().take() {
            *self.current.borrow_mut() = replacement;
        }
        Ok(())
    }
}

#[test]
fn replacement_indexes_and_renamed_tables_release_the_previous_table_lock() {
    for incarnation in [1, 2] {
        let session = Session::new(snapshot(incarnation, "public.moved"));
        let rows = session.bind(false, &mut Vec::new()).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].table_name, "public.moved");
        assert_eq!(session.refreshes.get(), 2);
        assert!(session.peer_acquires("public.t", RelationLockMode::AccessExclusive));
        assert!(!session.peer_acquires("public.moved", RelationLockMode::AccessShare));
        session.locks.release_session(1);
        assert!(session.peer_acquires("public.moved", RelationLockMode::AccessExclusive));
    }
}

#[test]
fn same_table_recreation_rebinds_the_index_incarnation_before_retaining_the_lock() {
    let session = Session::new(snapshot(2, "public.t"));
    let rows = session.bind(false, &mut Vec::new()).unwrap();
    assert_eq!(session.refreshes.get(), 2);
    assert_eq!(
        crate::catalog::index::index_definition(&rows[0])
            .unwrap()
            .catalog
            .unwrap()
            .identity
            .object_id,
        [2; 16]
    );
    assert!(!session.peer_acquires("public.t", RelationLockMode::AccessShare));
}

#[test]
fn changed_kind_and_missing_targets_keep_their_sqlstate() {
    for (change, expected) in [(0, "42809"), (1, "42704")] {
        let mut replacement = snapshot(2, "public.t");
        match change {
            0 => replacement.kind = "table",
            1 => replacement.row = None,
            _ => unreachable!(),
        }
        let session = Session::new(replacement);
        assert_eq!(
            session.bind(false, &mut Vec::new()).unwrap_err().sqlstate(),
            Some(expected)
        );
        assert!(session.peer_acquires("public.t", RelationLockMode::AccessExclusive));
    }
    let mut removed = snapshot(1, "public.t");
    removed.row = None;
    let session = Session::new(removed);
    let mut notices = Vec::new();
    assert!(session.bind(true, &mut notices).unwrap().is_empty());
    assert_eq!(notices, ["index \"idx\" does not exist, skipping"]);
    assert!(session.peer_acquires("public.t", RelationLockMode::AccessExclusive));
}

#[test]
fn constraint_index_targets_retain_the_current_table_before_dependency_validation() {
    let mut replacement = snapshot(2, "public.other");
    replacement.owned = true;
    replacement.row.as_mut().unwrap().definition_json = None;
    let session = Session::new(replacement);
    let rows = session.bind(false, &mut Vec::new()).unwrap();
    assert_eq!(rows[0].table_name, "public.other");
    assert!(session.has_constraint_index(&rows[0].relation));
    assert!(session.peer_acquires("public.t", RelationLockMode::AccessExclusive));
    assert!(!session.peer_acquires("public.other", RelationLockMode::AccessShare));
}

#[test]
fn revoked_authority_releases_only_the_provisional_upgrade() {
    let mut replacement = snapshot(1, "public.t");
    replacement.allowed = false;
    let session = Session::new(replacement);
    session
        .acquire("public.t", RelationLockMode::AccessShare, false)
        .unwrap()
        .unwrap()
        .retain();
    assert_eq!(
        session.bind(false, &mut Vec::new()).unwrap_err().sqlstate(),
        Some("42501")
    );
    assert!(session.peer_acquires("public.t", RelationLockMode::AccessShare));
    assert!(!session.peer_acquires("public.t", RelationLockMode::AccessExclusive));
}

#[test]
fn failed_refresh_preserves_its_diagnostic_and_releases_the_provisional_lock() {
    let mut session = Session::new(snapshot(1, "public.t"));
    session.failure = Some("40001");
    assert_eq!(
        session.bind(false, &mut Vec::new()).unwrap_err().sqlstate(),
        Some("40001")
    );
    assert!(session.peer_acquires("public.t", RelationLockMode::AccessExclusive));
}
