//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::Engine;
use parking_lot::MappedRwLockReadGuard;
use std::{
    cell::{Cell, RefCell},
    ops::Deref,
};
use uqa_core::RelationIdentity;
use uqa_execution::{
    catalog::security::table_grants::context::{TableGrantInputs, TableGrantNotices},
    schema::{
        foreign_table_alteration::{
            ForeignMemoryRegistryWrite, ForeignSecurityRegistryWrite, ForeignTableAlterPublication,
            ForeignTableRegistryWrite,
        },
        sequences::role_ownership::OwnedSequenceSecurityWrite,
    },
};
use uqa_sql::{
    ast::{GrantTableStmt, Statement},
    catalog::{
        roles::guards::{RoleCatalogGuards, RoleDefinitionRead, RoleMembershipRead},
        security::TableSecurity,
    },
    SQLError,
};
use uqa_storage::StorageBackendResult;

fn statement(sql: &str) -> GrantTableStmt {
    let Statement::GrantTable(statement) = uqa_sql::compile(sql).unwrap().remove(0) else {
        panic!("expected table grant")
    };
    statement
}
fn setup(engine: &Engine, foreign_column: &str) {
    engine.sql(&format!("CREATE ROLE reader; CREATE TABLE items(id integer); CREATE VIEW visible AS SELECT id FROM items; CREATE SERVER source FOREIGN DATA WRAPPER memory_fdw; CREATE FOREIGN TABLE remote({foreign_column} integer) SERVER source"),&[]).unwrap();
}
fn security(engine: &Engine) -> (TableSecurity, TableSecurity, TableSecurity) {
    let table = engine.storage.tables.read()[&RelationIdentity::new("public", "items")].security();
    let view = engine.durable.views.read()[&RelationIdentity::new("public", "visible")].security();
    let foreign = engine.durable.foreign_table_security.read()
        [&RelationIdentity::new("public", "remote")]
        .clone();
    (table, view, foreign)
}

struct Authorization<'a> {
    engine: &'a Engine,
    roles_held: Cell<bool>,
    memberships_held: Cell<bool>,
    calls: RefCell<Vec<&'static str>>,
}
impl<'a> Authorization<'a> {
    fn new(engine: &'a Engine) -> Self {
        Self {
            engine,
            roles_held: Cell::new(false),
            memberships_held: Cell::new(false),
            calls: RefCell::new(Vec::new()),
        }
    }
}
struct TrackedRead<'a, T> {
    guard: Option<MappedRwLockReadGuard<'a, T>>,
    held: &'a Cell<bool>,
    calls: &'a RefCell<Vec<&'static str>>,
    released: &'static str,
}
impl<T> Deref for TrackedRead<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        self.guard.as_deref().unwrap()
    }
}
impl<T> Drop for TrackedRead<'_, T> {
    fn drop(&mut self) {
        drop(self.guard.take());
        self.held.set(false);
        self.calls.borrow_mut().push(self.released);
    }
}
impl RoleCatalogGuards for Authorization<'_> {
    fn role_definitions(&self) -> RoleDefinitionRead<'_> {
        let guard = self.engine.durable.roles.read();
        assert!(!self.roles_held.replace(true));
        assert!(!self.memberships_held.get());
        self.calls.borrow_mut().push("roles");
        Box::new(TrackedRead {
            guard: Some(guard),
            held: &self.roles_held,
            calls: &self.calls,
            released: "roles-released",
        })
    }
    fn role_memberships(&self) -> RoleMembershipRead<'_> {
        let guard = self.engine.durable.role_memberships.read();
        assert!(self.roles_held.get());
        assert!(!self.memberships_held.replace(true));
        self.calls.borrow_mut().push("memberships");
        Box::new(TrackedRead {
            guard: Some(guard),
            held: &self.memberships_held,
            calls: &self.calls,
            released: "memberships-released",
        })
    }
}

struct FailedForeignWrite<'a> {
    authorization: &'a Authorization<'a>,
    before: (TableSecurity, TableSecurity, TableSecurity),
    reached: Cell<bool>,
}
impl ForeignTableAlterPublication for FailedForeignWrite<'_> {
    fn persist_rename(
        &self,
        from: &RelationIdentity,
        to: &RelationIdentity,
    ) -> StorageBackendResult<Option<bool>> {
        ForeignTableAlterPublication::persist_rename(self.authorization.engine, from, to)
    }
    fn tables_write(&self) -> ForeignTableRegistryWrite<'_> {
        ForeignTableAlterPublication::tables_write(self.authorization.engine)
    }
    fn security_write(&self) -> ForeignSecurityRegistryWrite<'_> {
        ForeignTableAlterPublication::security_write(self.authorization.engine)
    }
    fn memory_tables_write(&self) -> ForeignMemoryRegistryWrite<'_> {
        ForeignTableAlterPublication::memory_tables_write(self.authorization.engine)
    }
    fn sequence_security_write(&self) -> OwnedSequenceSecurityWrite<'_> {
        ForeignTableAlterPublication::sequence_security_write(self.authorization.engine)
    }
    fn persist_security(
        &self,
        _relation: &RelationIdentity,
        _security: &TableSecurity,
    ) -> Result<(), SQLError> {
        assert!(self.authorization.roles_held.get() && self.authorization.memberships_held.get());
        assert_eq!(security(self.authorization.engine), self.before);
        let catalog = self.authorization.engine.storage.catalog.as_ref().unwrap();
        assert!(catalog.load_tables().unwrap()[0].acl.is_some());
        assert!(catalog.load_views().unwrap()[0].acl.is_some());
        self.reached.set(true);
        Err(SQLError::Internal(
            "injected foreign ACL persistence failure".into(),
        ))
    }
}

#[test]
fn mixed_grant_validation_failure_preserves_all_relation_security() {
    let engine = Engine::new();
    setup(&engine, "other");
    let before = security(&engine);
    let error = engine
        .sql("GRANT UPDATE(id) ON items, visible, remote TO reader", &[])
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("42703"));
    assert_eq!(security(&engine), before);
}

#[test]
fn final_foreign_persistence_failure_rolls_back_prior_writes_without_publishing_acl_candidates() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("mixed_grant.db");
    let engine = Engine::open(&path).unwrap();
    setup(&engine, "id");
    let before = security(&engine);
    {
        let authorization = Authorization::new(&engine);
        let publication = FailedForeignWrite {
            authorization: &authorization,
            before: before.clone(),
            reached: Cell::new(false),
        };
        let statement = statement("GRANT SELECT ON items, visible, remote TO reader");
        let error = engine
            .with_implicit_transaction(|engine| {
                let mut context = engine.table_grant_context();
                context.roles = &authorization;
                context.foreign = &publication;
                context.grant_table_privileges(&statement)
            })
            .unwrap_err();
        assert!(error
            .to_string()
            .contains("injected foreign ACL persistence failure"));
        assert!(publication.reached.get());
        assert!(!authorization.roles_held.get() && !authorization.memberships_held.get());
        assert_eq!(security(&engine), before);
        let catalog = engine.storage.catalog.as_ref().unwrap();
        assert!(catalog.load_tables().unwrap()[0].acl.is_none());
        assert!(catalog.load_views().unwrap()[0].acl.is_none());
        assert!(catalog.load_foreign_tables().unwrap()[0].acl.is_none());
    }
    drop(engine);
    let reopened = Engine::open(&path).unwrap();
    assert_eq!(security(&reopened), before);
}

impl TableGrantNotices for Authorization<'_> {
    fn notice(&self, level: &str, message: &str) {
        assert!(!self.roles_held.get() && !self.memberships_held.get());
        assert!(
            self.engine.storage.tables.read()[&RelationIdentity::new("public", "items")]
                .security()
                .acl
                .is_some()
        );
        assert!(self.engine.durable.sequence_security.read()
            [&RelationIdentity::new("public", "ids")]
            .acl
            .is_none());
        self.calls.borrow_mut().push("notice");
        self.engine.push_sql_notice(level, message);
    }
}

#[test]
fn mixed_table_sequence_grant_releases_authorization_guards_before_sequence_warnings() {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE ROLE reader; CREATE TABLE items(id integer); CREATE SEQUENCE ids",
            &[],
        )
        .unwrap();
    let authorization = Authorization::new(&engine);
    let statement = statement("GRANT SELECT, INSERT ON TABLE items, ids TO reader");
    engine
        .with_implicit_transaction(|engine| {
            let mut context = engine.table_grant_context();
            context.roles = &authorization;
            context.notices = &authorization;
            context.grant_table_privileges(&statement)
        })
        .unwrap();
    assert_eq!(
        *authorization.calls.borrow(),
        vec![
            "roles",
            "memberships",
            "memberships-released",
            "roles-released",
            "notice"
        ]
    );
    assert_eq!(
        engine.take_sql_notices(),
        vec![(
            "WARNING".into(),
            "sequence \"ids\" only supports USAGE, SELECT, and UPDATE privileges".into()
        )]
    );
    assert!(
        engine.durable.sequence_security.read()[&RelationIdentity::new("public", "ids")]
            .acl
            .is_some()
    );
}
