//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::{cell::RefCell, collections::BTreeMap};
use uqa_sql::catalog::roles::{
    guards::{RoleDefinitionRead, RoleMembershipRead},
    RoleDefinition, RoleMembership, RoleMembershipKey,
};

struct Fixture {
    tables: RefCell<BTreeMap<String, [u8; 16]>>,
    children: Vec<String>,
    replace_after_wait: RefCell<Option<(String, Option<[u8; 16]>)>>,
    rename_after_acquire: RefCell<Option<(&'static str, &'static str)>>,
    fail: Option<&'static str>,
    acquired: RefCell<Vec<String>>,
    roles: BTreeMap<String, RoleDefinition>,
    memberships: BTreeMap<RoleMembershipKey, RoleMembership>,
    manager: crate::row_locks::RowLockManager,
    cancel: uqa_core::CancellationToken,
}

impl Fixture {
    fn new() -> Self {
        Self {
            tables: RefCell::new(BTreeMap::from([
                ("public.a".into(), [1; 16]),
                ("public.b".into(), [2; 16]),
            ])),
            children: Vec::new(),
            replace_after_wait: RefCell::new(None),
            rename_after_acquire: RefCell::new(None),
            fail: None,
            acquired: RefCell::new(Vec::new()),
            roles: BTreeMap::from([("uqa".into(), RoleDefinition::bootstrap())]),
            memberships: BTreeMap::new(),
            manager: crate::row_locks::RowLockManager::new(),
            cancel: uqa_core::CancellationToken::new(),
        }
    }
    fn execute(&self, sql: &str) -> Result<SQLResult, SQLError> {
        let uqa_sql::Statement::LockTable(statement) = uqa_sql::compile(sql)?.remove(0) else {
            panic!("expected lock")
        };
        execute(
            TableLockContext {
                catalog: self,
                roles: self,
                session: self,
            },
            &statement,
            false,
        )
    }
}

impl RoleCatalogGuards for Fixture {
    fn role_definitions(&self) -> RoleDefinitionRead<'_> {
        Box::new(&self.roles)
    }
    fn role_memberships(&self) -> RoleMembershipRead<'_> {
        Box::new(&self.memberships)
    }
}

impl TableLockCatalog for Fixture {
    fn resolve(&self, name: &str, _: bool) -> Result<RelationResolution, SQLError> {
        let canonical = if name.contains('.') {
            name.into()
        } else {
            format!("public.{name}")
        };
        Ok(if self.tables.borrow().contains_key(&canonical) {
            RelationResolution::Found(canonical, "table")
        } else {
            RelationResolution::MissingRelation
        })
    }
    fn table(&self, name: &str) -> Result<Option<TableLockMetadata>, SQLError> {
        Ok(self.tables.borrow().get(name).map(|id| TableLockMetadata {
            object_id: *id,
            security: TableSecurity::owner("uqa"),
        }))
    }
    fn view(&self, _: &str) -> Result<Option<StoredView>, SQLError> {
        Ok(None)
    }
    fn table_name(&self, object_id: [u8; 16]) -> Option<String> {
        self.tables
            .borrow()
            .iter()
            .find_map(|(name, id)| (*id == object_id).then(|| name.clone()))
    }
    fn descendants(&self, _: &str) -> Result<Vec<String>, SQLError> {
        Ok(self.children.clone())
    }
}

impl TableLockSession for Fixture {
    fn in_transaction_block(&self) -> bool {
        true
    }
    fn current_user(&self) -> String {
        "uqa".into()
    }
    fn acquire(
        &self,
        name: &str,
        mode: RelationLockMode,
        nowait: bool,
    ) -> Result<Option<ScopedRelationLock<'_>>, SQLError> {
        self.acquired.borrow_mut().push(name.into());
        let rename = *self.rename_after_acquire.borrow();
        if let Some((from, to)) = rename.filter(|(from, _)| *from == name) {
            self.rename_after_acquire.borrow_mut().take();
            let id = self.tables.borrow_mut().remove(from).unwrap();
            self.tables.borrow_mut().insert(to.into(), id);
            self.tables.borrow_mut().insert(from.into(), [9; 16]);
        }
        if self.fail.is_some_and(|name_to_fail| name == name_to_fail) {
            self.tables.borrow_mut().remove(name);
            return Err(SQLError::Routine {
                sqlstate: "57014".into(),
                message: "cancelled acquisition".into(),
            });
        }
        let key = self.manager.table_key(name);
        if nowait {
            self.manager
                .try_acquire_scoped_relation(1, key, mode, (0, 1), &self.cancel)
        } else {
            self.manager
                .acquire_scoped_relation(1, key, mode, (0, 1), &self.cancel)
                .map(Some)
        }
    }
    fn refresh_after_wait(&self) -> Result<(), SQLError> {
        if let Some((name, next)) = self.replace_after_wait.borrow_mut().take() {
            if let Some(next) = next {
                self.tables.borrow_mut().insert(name, next);
            } else {
                self.tables.borrow_mut().remove(&name);
            }
        }
        Ok(())
    }
}

#[test]
fn names_lock_in_statement_order_and_replacement_is_rebound_before_retention() {
    let fixture = Fixture::new();
    *fixture.replace_after_wait.borrow_mut() = Some(("public.a".into(), Some([3; 16])));
    fixture.execute("LOCK a, b IN SHARE MODE NOWAIT").unwrap();
    assert_eq!(
        *fixture.acquired.borrow(),
        ["public.a", "public.a", "public.b"]
    );
    for name in ["public.a", "public.b"] {
        assert!(!fixture
            .manager
            .try_acquire_relation(
                2,
                fixture.manager.table_key(name),
                RelationLockMode::RowExclusive,
                0,
                &fixture.cancel
            )
            .unwrap());
    }
    fixture.manager.release_session(1);
    assert!(fixture
        .manager
        .try_acquire_relation(
            2,
            fixture.manager.table_key("public.a"),
            RelationLockMode::AccessExclusive,
            0,
            &fixture.cancel
        )
        .unwrap());
}

#[test]
fn a_disappeared_binding_releases_its_provisional_lock() {
    let fixture = Fixture::new();
    *fixture.replace_after_wait.borrow_mut() = Some(("public.a".into(), None));
    assert_eq!(
        fixture.execute("LOCK a").unwrap_err().sqlstate(),
        Some("42P01")
    );
    assert!(fixture
        .manager
        .try_acquire_relation(
            2,
            fixture.manager.table_key("public.a"),
            RelationLockMode::AccessExclusive,
            0,
            &fixture.cancel
        )
        .unwrap());
}

#[test]
fn descendant_disappearance_does_not_swallow_acquisition_cancellation() {
    let mut fixture = Fixture::new();
    fixture.children = vec!["public.a".into(), "public.b".into()];
    fixture.fail = Some("public.b");
    assert_eq!(
        fixture.execute("LOCK a").unwrap_err().sqlstate(),
        Some("57014")
    );
    assert_eq!(*fixture.acquired.borrow(), ["public.a", "public.b"]);
}

#[test]
fn descendant_locks_follow_object_identity_across_rename_and_name_reuse() {
    let mut fixture = Fixture::new();
    fixture.children = vec!["public.a".into(), "public.b".into()];
    *fixture.rename_after_acquire.borrow_mut() = Some(("public.b", "public.renamed"));
    fixture.execute("LOCK a IN SHARE MODE NOWAIT").unwrap();
    assert_eq!(
        *fixture.acquired.borrow(),
        ["public.a", "public.b", "public.renamed"]
    );
    assert!(!fixture
        .manager
        .try_acquire_relation(
            2,
            fixture.manager.table_key("public.renamed"),
            RelationLockMode::RowExclusive,
            0,
            &fixture.cancel,
        )
        .unwrap());
    assert!(fixture
        .manager
        .try_acquire_relation(
            2,
            fixture.manager.table_key("public.b"),
            RelationLockMode::AccessExclusive,
            0,
            &fixture.cancel,
        )
        .unwrap());
}
