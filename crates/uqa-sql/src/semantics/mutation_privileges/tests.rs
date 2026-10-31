//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    catalog::{roles::RoleIdentity, stored_view::StoredView},
    plan::{CommandPlan, QueryPlan, RelationalPlan, UnifiedPlan},
    semantics::privileges::TargetSelectPrivilegeRequest,
};
use std::cell::{Cell, RefCell};
use uqa_core::{
    memory::{MemoryBudget, Produced},
    CancellationToken,
};

struct Catalog {
    columns: Vec<String>,
    calls: RefCell<Vec<&'static str>>,
    subject: Cell<Option<*const RoleReference>>,
    select_count: Cell<usize>,
    fail_select: Cell<bool>,
    unwind: Cell<bool>,
}
impl Catalog {
    fn new() -> Self {
        Self {
            columns: vec!["id".into(), "value".into(), "x".repeat(4 << 20)],
            calls: RefCell::new(Vec::with_capacity(64)),
            subject: Cell::new(None),
            select_count: Cell::new(0),
            fail_select: Cell::new(false),
            unwind: Cell::new(false),
        }
    }
    fn observe(&self, subject: &RoleReference, event: &'static str) {
        if let Some(expected) = self.subject.get() {
            assert_eq!(std::ptr::from_ref(subject), expected);
        }
        self.calls.borrow_mut().push(event);
    }
}
impl MutationPrivilegeCatalog for Catalog {
    fn bound_table_column_names(&self, _: &str) -> Result<Vec<String>, SQLError> {
        panic!("selected native declaration must be borrowed")
    }
    fn visit_bound_table_column_names_with_control(
        &self,
        _: &str,
        control: &ProductionControl<'_>,
        read: &mut dyn FnMut(&str) -> Result<(), SQLError>,
    ) -> Result<(), SQLError> {
        control.check()?;
        self.calls.borrow_mut().push("columns");
        for name in &self.columns {
            read(name)?;
        }
        Ok(())
    }
    fn ensure_table_privilege_for(
        &self,
        _: &str,
        subject: &RoleReference,
        privilege: TableAclPrivilege,
    ) -> Result<(), SQLError> {
        assert_eq!(privilege, TableAclPrivilege::Delete);
        self.observe(subject, "delete");
        Ok(())
    }
    fn ensure_column_privilege_for(
        &self,
        _: &str,
        column: &str,
        subject: &RoleReference,
        privilege: TableAclPrivilege,
    ) -> Result<(), SQLError> {
        self.observe(
            subject,
            match (privilege, column) {
                (TableAclPrivilege::Insert, "id") => "insert:id",
                (TableAclPrivilege::Insert, "value") => "insert:value",
                (TableAclPrivilege::Update, "id") => "update:id",
                (TableAclPrivilege::Update, "value") => "update:value",
                _ => "other",
            },
        );
        Ok(())
    }
    fn ensure_any_column_privilege_for(
        &self,
        _: &str,
        subject: &RoleReference,
        privilege: TableAclPrivilege,
    ) -> Result<(), SQLError> {
        assert_eq!(privilege, TableAclPrivilege::Insert);
        self.observe(subject, "insert:any");
        Ok(())
    }
}
impl ViewPrivilegeCatalog for Catalog {
    fn current_role_with_control(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<RoleReference>, SQLError> {
        self.calls.borrow_mut().push("role");
        let (name, memory) = control.copy_text("reader")?.into_parts();
        Ok(control.finish(RoleReference::Named(name), memory)?)
    }
    fn current_role(&self) -> RoleReference {
        panic!("controlled role capture")
    }
    fn bound_role(&self, _: RoleIdentity) -> Result<RoleReference, SQLError> {
        panic!("table subject")
    }
    fn view_definition(&self, _: &str) -> Result<Option<StoredView>, SQLError> {
        panic!("table target")
    }
    fn ensure_query_select(&self, _: &QueryPlan) -> Result<(), SQLError> {
        panic!("target expressions")
    }
    fn ensure_view_privilege_for(
        &self,
        _: &str,
        _: &StoredView,
        _: &RoleReference,
        _: TableAclPrivilege,
    ) -> Result<(), SQLError> {
        panic!("table target")
    }
    fn ensure_view_column_privilege_for(
        &self,
        _: &str,
        _: &StoredView,
        _: &str,
        _: &RoleReference,
        _: TableAclPrivilege,
    ) -> Result<(), SQLError> {
        panic!("table target")
    }
    fn ensure_any_view_column_privilege_for(
        &self,
        _: &str,
        _: &StoredView,
        _: &RoleReference,
        _: TableAclPrivilege,
    ) -> Result<(), SQLError> {
        panic!("table target")
    }
    fn ensure_target_select(
        &self,
        request: TargetSelectPrivilegeRequest<'_, '_>,
    ) -> Result<(), SQLError> {
        self.calls.borrow_mut().push("select");
        self.select_count.set(request.expressions.len());
        assert!(!self.unwind.get(), "selected SELECT callback unwind");
        if self.fail_select.get() {
            return Err(SQLError::Routine {
                sqlstate: "XX005".into(),
                message: "selected SELECT failure".into(),
            });
        }
        Ok(())
    }
}
fn command(sql: &str) -> Box<CommandPlan> {
    let UnifiedPlan::Command(command) = UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0))
    else {
        panic!("command");
    };
    command
}

#[test]
fn mutation_target_names_admit_only_the_selected_prefix_and_ignore_output_labels() {
    let catalog = Catalog::new();
    let mut input = command("INSERT INTO items SELECT 1 AS id");
    let CommandPlan::Insert(statement) = &mut *input else {
        panic!("insert");
    };
    let RelationalPlan::QueryBlock(block) = &mut statement.source.as_mut().unwrap().root else {
        panic!("query");
    };
    block.projections[0].alias = Some("x".repeat(4 << 20));
    let memory = MemoryBudget::new(16 << 10);
    let statement_token = CancellationToken::new();
    let session_token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &statement_token, &session_token);
    let sibling = memory.reserve(13).unwrap();
    let mut output = None;
    let observed = allocation_counter::measure(|| {
        output = Some(insert_target_columns_with_control(&catalog, statement, &control).unwrap());
    });
    let output = output.unwrap();
    assert_eq!(output.as_slice(), ["id"]);
    assert_eq!(
        output.reserved_bytes(),
        usize::try_from(observed.bytes_current).unwrap()
    );
    assert_eq!(memory.used(), sibling.bytes() + output.reserved_bytes());
    assert!(observed.bytes_total < 1024, "{observed:?}");
    assert_eq!(*catalog.calls.borrow(), ["columns"]);
    drop(output);
    assert_eq!(memory.used(), sibling.bytes());
    statement.columns = vec![crate::ast::AssignmentTarget::from("x".repeat(4 << 20))];
    let observed = allocation_counter::measure(|| {
        assert_eq!(
            insert_target_columns_with_control(&catalog, statement, &control)
                .unwrap_err()
                .sqlstate(),
            Some("53200")
        );
    });
    assert!(observed.bytes_total < 1024, "{observed:?}");
    assert_eq!(memory.used(), sibling.bytes());
    assert_eq!(*catalog.calls.borrow(), ["columns"]);
}

#[test]
fn mutation_authorization_borrows_explicit_subjects_and_retains_expression_references() {
    let catalog = Catalog::new();
    let mut input = command("UPDATE items SET value=1 WHERE id=2 RETURNING id");
    let CommandPlan::Update(statement) = &mut *input else {
        panic!("update");
    };
    statement.target_privilege_subject = Some(RoleReference::Named("x".repeat(4 << 20)));
    catalog.subject.set(Some(std::ptr::from_ref(
        statement.target_privilege_subject.as_ref().unwrap(),
    )));
    let memory = MemoryBudget::new(16 << 10);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let mut output = None;
    let observed = allocation_counter::measure(|| {
        output = Some(
            ensure_update_target_privileges_with_control(&catalog, statement, &control).unwrap(),
        );
    });
    let output = output.unwrap();
    assert_eq!(output.len(), 3);
    assert!(std::ptr::eq(
        output[0],
        std::ptr::from_ref(&statement.assignments[0].value)
    ));
    assert!(std::ptr::eq(
        output[1],
        statement.predicate.as_ref().unwrap()
    ));
    assert!(std::ptr::eq(
        output[2],
        std::ptr::from_ref(&statement.returning[0].expr)
    ));
    assert_eq!(
        output.reserved_bytes(),
        usize::try_from(observed.bytes_current).unwrap()
    );
    assert_eq!(memory.used(), output.reserved_bytes());
    assert!(observed.bytes_total < 1024, "{observed:?}");
    assert_eq!(*catalog.calls.borrow(), ["update:value", "select"]);
    drop(output);
    assert_eq!(memory.used(), 0);
}

#[test]
fn mutation_authorization_preserves_defaults_conflict_and_merge_callback_order() {
    let catalog = Catalog::new();
    let memory = MemoryBudget::new(16 << 10);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    for (sql, expected) in [
        ("INSERT INTO items DEFAULT VALUES", vec!["role", "insert:any", "select"]),
        ("INSERT INTO items(id,value) VALUES(1,2) ON CONFLICT(id) DO UPDATE SET value=3 RETURNING id", vec!["role", "insert:id", "insert:value", "update:value", "select"]),
    ] {
        let input = command(sql);
        let CommandPlan::Insert(statement) = &*input else { panic!("insert"); };
        let conflicts = conflict_update_columns_with_control(statement, &control).unwrap();
        ensure_insert_target_privileges_with_control(&catalog, statement, conflicts.as_ref().map(|names| names.as_slice()), &control).unwrap();
        assert_eq!(*catalog.calls.borrow(), expected);
        catalog.calls.borrow_mut().clear();
        drop(conflicts);
        assert_eq!(memory.used(), 0);
    }
    let input = command("MERGE INTO items USING (VALUES(1)) s(id) ON items.id=s.id WHEN MATCHED AND s.id=1 THEN UPDATE SET value=2,id=3 WHEN MATCHED THEN DELETE WHEN NOT MATCHED THEN INSERT(value,id) VALUES(2,s.id)");
    let CommandPlan::Merge(statement) = &*input else {
        panic!("merge");
    };
    merge::ensure_merge_mutation_privileges_with_control(&catalog, statement, &control).unwrap();
    assert_eq!(
        *catalog.calls.borrow(),
        [
            "columns",
            "role",
            "delete",
            "insert:id",
            "insert:value",
            "update:id",
            "update:value"
        ]
    );
    assert_eq!(memory.used(), 0);
    let names = merge::merge_target_columns_with_control(&catalog, statement, &control).unwrap();
    assert_eq!(names.as_slice(), ["value", "id", "value", "id"]);
    drop(names);
    let names = merge::merge_update_columns_with_control(statement, &control).unwrap();
    assert_eq!(names.as_slice(), ["id", "value"]);
    drop(names);
    assert_eq!(memory.used(), 0);
}

#[test]
fn mutation_authorization_releases_partial_outputs_errors_cancellation_and_unwind() {
    let catalog = Catalog::new();
    let input = command("DELETE FROM items WHERE id=1 RETURNING id,value");
    let CommandPlan::Delete(statement) = &*input else {
        panic!("delete");
    };
    let memory = MemoryBudget::new(16 << 10);
    let statement_token = CancellationToken::new();
    let session_token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &statement_token, &session_token);
    let sibling = memory.reserve(13).unwrap();
    for available in [0, 4, 16, 32] {
        let occupied = memory.reserve(memory.available() - available).unwrap();
        assert_eq!(
            ensure_delete_target_privileges_with_control(&catalog, statement, &control)
                .unwrap_err()
                .sqlstate(),
            Some("53200")
        );
        assert_eq!(memory.used(), sibling.bytes() + occupied.bytes());
        drop(occupied);
    }
    for token in [&statement_token, &session_token] {
        token.cancel();
        assert_eq!(
            ensure_delete_target_privileges_with_control(&catalog, statement, &control)
                .unwrap_err()
                .sqlstate(),
            Some("57014")
        );
        assert_eq!(memory.used(), sibling.bytes());
        token.reset();
    }
    catalog.fail_select.set(true);
    assert_eq!(
        ensure_delete_target_privileges_with_control(&catalog, statement, &control)
            .unwrap_err()
            .sqlstate(),
        Some("XX005")
    );
    assert_eq!(memory.used(), sibling.bytes());
    catalog.fail_select.set(false);
    catalog.unwind.set(true);
    assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        ensure_delete_target_privileges_with_control(&catalog, statement, &control)
    }))
    .is_err());
    assert_eq!(memory.used(), sibling.bytes());
    catalog.unwind.set(false);
    drop(ensure_delete_target_privileges_with_control(&catalog, statement, &control).unwrap());
    assert_eq!(memory.used(), sibling.bytes());
}
