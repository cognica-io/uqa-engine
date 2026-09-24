//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Target binding and lock waits preserve relation identity and hierarchy lifetime.

use super::*;
use crate::row_locks::RowLockManager;
use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
};

#[derive(Default)]
struct Fixture {
    tables: RefCell<Vec<AnalyzeTarget>>,
    changes: RefCell<VecDeque<Option<Vec<AnalyzeTarget>>>>,
    children: Vec<String>,
    acquired: RefCell<Vec<(String, RelationLockMode)>>,
    notices: RefCell<Vec<String>>,
    authorized: RefCell<Vec<String>>,
    manager: RowLockManager,
    denied: Cell<bool>,
}

fn target(name: &str, object: u8) -> AnalyzeTarget {
    AnalyzeTarget {
        name: name.into(),
        object_id: [object; 16],
    }
}

impl Fixture {
    fn context(&self) -> AnalyzeContext<'_> {
        AnalyzeContext {
            catalog: self,
            locks: self,
            notices: self,
            privileges: Some(self),
        }
    }
}

impl AnalyzeCatalog for Fixture {
    fn resolve(&self, name: &str) -> Result<Option<AnalyzeTarget>, SQLError> {
        Ok(self
            .tables
            .borrow()
            .iter()
            .find(|table| table.name == name)
            .cloned())
    }
    fn all_tables(&self) -> Vec<AnalyzeTarget> {
        self.tables.borrow().clone()
    }
    fn current_target(&self, object_id: [u8; 16]) -> Option<AnalyzeTarget> {
        self.tables
            .borrow()
            .iter()
            .find(|table| table.object_id == object_id)
            .cloned()
    }
    fn descendants(&self, _name: &str) -> Result<Vec<String>, SQLError> {
        Ok(self.children.clone())
    }
}

impl AnalyzeLocks for Fixture {
    fn bind_name(&self, name: &str) -> Result<ScopedRelationLock<'_>, SQLError> {
        self.acquired
            .borrow_mut()
            .push((name.into(), RelationLockMode::AccessShare));
        self.manager.acquire_scoped_relation(
            1,
            self.manager.table_key(name),
            RelationLockMode::AccessShare,
            (0, 1),
            &uqa_core::CancellationToken::new(),
        )
    }
    fn acquire(&self, name: &str, mode: RelationLockMode) -> Result<(), SQLError> {
        self.acquired.borrow_mut().push((name.into(), mode));
        Ok(())
    }
    fn refresh_after_wait(&self) -> Result<(), SQLError> {
        if let Some(Some(tables)) = self.changes.borrow_mut().pop_front() {
            *self.tables.borrow_mut() = tables;
        }
        Ok(())
    }
}

impl AnalyzeNotices for Fixture {
    fn warning(&self, message: &str) {
        self.notices.borrow_mut().push(message.into());
    }
}

impl VacuumPrivileges for Fixture {
    fn ensure_maintain(&self, table: &str) -> Result<(), SQLError> {
        if self.denied.get() {
            return Err(SQLError::Routine {
                sqlstate: "42501".into(),
                message: "permission denied".into(),
            });
        }
        self.authorized.borrow_mut().push(table.into());
        Ok(())
    }
}

#[test]
fn initial_name_binding_follows_replacement_before_the_object_is_selected() {
    let fixture = Fixture::default();
    *fixture.tables.borrow_mut() = vec![target("t", 1)];
    fixture
        .changes
        .borrow_mut()
        .push_back(Some(vec![target("t", 2)]));
    let selected = prepare_targets(&fixture.context(), Some("t"), false).unwrap();
    assert_eq!(selected, vec![target("t", 2)]);
    assert_eq!(
        *fixture.acquired.borrow(),
        vec![
            ("t".into(), RelationLockMode::AccessShare),
            ("t".into(), RelationLockMode::AccessShare),
            ("t".into(), RelationLockMode::ShareUpdateExclusive),
        ]
    );
    assert_eq!(*fixture.authorized.borrow(), ["t"]);
}

#[test]
fn an_object_renamed_after_binding_is_locked_again_under_its_current_name() {
    let fixture = Fixture::default();
    *fixture.tables.borrow_mut() = vec![target("old", 1)];
    fixture
        .changes
        .borrow_mut()
        .extend([None, Some(vec![target("new", 1)])]);
    let selected = prepare_targets(&fixture.context(), Some("old"), false).unwrap();
    assert_eq!(selected, vec![target("new", 1)]);
    assert_eq!(
        fixture.acquired.borrow().last().unwrap(),
        &("new".into(), RelationLockMode::ShareUpdateExclusive)
    );
    let mut analyzed = Vec::new();
    run_locked_targets(&selected, |name| {
        analyzed.push(name.to_owned());
        Ok(())
    })
    .unwrap();
    assert_eq!(analyzed, ["new"]);
    assert_eq!(*fixture.authorized.borrow(), ["new"]);
}

#[test]
fn replacement_after_binding_is_skipped_instead_of_analyzing_the_new_object() {
    let fixture = Fixture::default();
    *fixture.tables.borrow_mut() = vec![target("t", 1)];
    fixture
        .changes
        .borrow_mut()
        .extend([None, Some(vec![target("t", 2)])]);
    let selected = prepare_targets(&fixture.context(), Some("t"), false).unwrap();
    assert!(selected.is_empty());
    run_locked_targets(&selected, |_| panic!("a replacement must not be analyzed")).unwrap();
    assert_eq!(fixture.notices.borrow().len(), 1);
    assert!(fixture.authorized.borrow().is_empty());
}

#[test]
fn hierarchy_sources_take_access_share_while_only_the_statistics_target_takes_exclusive_analysis() {
    let fixture = Fixture {
        children: vec!["parent".into(), "child".into()],
        ..Fixture::default()
    };
    *fixture.tables.borrow_mut() = vec![target("parent", 1), target("child", 2)];
    let selected = prepare_targets(&fixture.context(), Some("parent"), true).unwrap();
    assert_eq!(selected, vec![target("parent", 1)]);
    assert_eq!(
        *fixture.acquired.borrow(),
        vec![
            ("parent".into(), RelationLockMode::AccessShare),
            ("parent".into(), RelationLockMode::ShareUpdateExclusive),
            ("child".into(), RelationLockMode::AccessShare),
        ]
    );
}

#[test]
fn missing_targets_and_post_wait_privileges_preserve_their_sql_errors() {
    let fixture = Fixture::default();
    assert_eq!(
        prepare_targets(&fixture.context(), Some("t"), false)
            .unwrap_err()
            .sqlstate(),
        Some("42P01")
    );
    assert!(prepare_optional_target(&fixture.context(), "t")
        .unwrap()
        .is_none());
    *fixture.tables.borrow_mut() = vec![target("t", 1)];
    fixture.denied.set(true);
    assert_eq!(
        prepare_targets(&fixture.context(), Some("t"), false)
            .unwrap_err()
            .sqlstate(),
        Some("42501")
    );
    assert_eq!(
        fixture.acquired.borrow().last().unwrap().1,
        RelationLockMode::ShareUpdateExclusive
    );
}
