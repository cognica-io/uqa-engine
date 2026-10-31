//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    ast::RelationPersistence,
    catalog::{
        roles::RoleIdentity, security::BoundTableSecurity, stored_view::StoredViewDefinition,
        view::StoredViewKind,
    },
    plan::{CommandPlan, QueryPlan, RelationalPlan, UnifiedPlan},
    ScalarExpr,
};
use std::cell::{Cell, RefCell};
use uqa_core::{memory::MemoryBudget, CancellationToken, Value};

struct Catalog {
    view: StoredView,
    calls: RefCell<Vec<String>>,
    borrow_calls: Cell<usize>,
    fail_select: Cell<bool>,
    unwind: Cell<bool>,
}
impl Catalog {
    fn new() -> Self {
        let UnifiedPlan::Query(mut query) = plan("SELECT 1 AS id") else {
            panic!("query")
        };
        let RelationalPlan::QueryBlock(block) = &mut query.root else {
            panic!("block")
        };
        block.projections[0].expr = ScalarExpr::Literal(Value::Str("x".repeat(4 << 20)));
        Self {
            view: StoredView {
                security: BoundTableSecurity::owner(RoleIdentity {
                    oid: 42,
                    object_id: [42; 16],
                }),
                definition: StoredViewDefinition {
                    object_id: [7; 16],
                    catalog_oids: None,
                    row_type_array_name: None,
                    query: *query,
                    output_columns: Some(vec!["id".into(), "value".into()]),
                    persistence: RelationPersistence::Permanent,
                    options: vec![("security_invoker".into(), "true".into())],
                    kind: StoredViewKind::View,
                    materialized_rows: Vec::new(),
                    materialized_column_types: Vec::new(),
                    materialized_column_collations: None,
                    populated: true,
                },
            },
            calls: RefCell::new(Vec::new()),
            borrow_calls: Cell::new(0),
            fail_select: Cell::new(false),
            unwind: Cell::new(false),
        }
    }
}
impl ViewPrivilegeCatalog for Catalog {
    fn with_view_definition_with_control(
        &self,
        _: &str,
        control: &ProductionControl<'_>,
        read: &mut dyn FnMut(Option<&StoredView>) -> Result<(), SQLError>,
    ) -> Result<(), SQLError> {
        control.check()?;
        self.borrow_calls.set(self.borrow_calls.get() + 1);
        read(Some(&self.view))
    }
    fn view_definition(&self, _: &str) -> Result<Option<StoredView>, SQLError> {
        Ok(Some(self.view.clone()))
    }
    fn current_role_with_control(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<RoleReference>, SQLError> {
        named("reader", control)
    }
    fn bound_role_with_control(
        &self,
        _: RoleIdentity,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<RoleReference>, SQLError> {
        named("owner", control)
    }
    fn current_role(&self) -> RoleReference {
        "reader".into()
    }
    fn bound_role(&self, _: RoleIdentity) -> Result<RoleReference, SQLError> {
        Ok("owner".into())
    }
    fn ensure_query_select(&self, _: &QueryPlan) -> Result<(), SQLError> {
        panic!("view authorization does not plan the stored query")
    }
    fn ensure_view_privilege_for(
        &self,
        _: &str,
        _: &StoredView,
        _: &RoleReference,
        privilege: TableAclPrivilege,
    ) -> Result<(), SQLError> {
        self.calls.borrow_mut().push(format!("{privilege:?}"));
        Ok(())
    }
    fn ensure_view_column_privilege_for(
        &self,
        _: &str,
        _: &StoredView,
        column: &str,
        _: &RoleReference,
        privilege: TableAclPrivilege,
    ) -> Result<(), SQLError> {
        self.calls
            .borrow_mut()
            .push(format!("{privilege:?}:{column}"));
        Ok(())
    }
    fn ensure_any_view_column_privilege_for(
        &self,
        _: &str,
        _: &StoredView,
        _: &RoleReference,
        privilege: TableAclPrivilege,
    ) -> Result<(), SQLError> {
        self.calls.borrow_mut().push(format!("{privilege:?}:any"));
        Ok(())
    }
    fn ensure_target_select(
        &self,
        request: TargetSelectPrivilegeRequest<'_, '_>,
    ) -> Result<(), SQLError> {
        assert!(!self.unwind.get(), "selected authorization callback unwind");
        if self.fail_select.get() {
            return Err(SQLError::Routine {
                sqlstate: "XX005".into(),
                message: "selected authorization failure".into(),
            });
        }
        self.calls.borrow_mut().push(format!(
            "select:{}:{}",
            request.expressions.len(),
            request.required_columns.join(",")
        ));
        Ok(())
    }
}
fn plan(sql: &str) -> UnifiedPlan {
    UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0))
}
fn check(catalog: &Catalog, plan: &UnifiedPlan) -> Result<RoleReference, SQLError> {
    let UnifiedPlan::Command(command) = plan else {
        panic!("command")
    };
    match &**command {
        CommandPlan::Insert(statement) => ensure_insert(catalog, statement),
        CommandPlan::Update(statement) => ensure_update(catalog, statement),
        CommandPlan::Delete(statement) => ensure_delete(catalog, statement),
        CommandPlan::Merge(statement) => ensure_merge(catalog, statement),
        _ => panic!("mutation"),
    }
}

#[test]
fn view_mutation_authorization_borrows_unused_definition_payloads() {
    let catalog = Catalog::new();
    for sql in [
        "INSERT INTO v(id) VALUES(1) RETURNING id",
        "UPDATE v SET id = 2 WHERE id = 1 RETURNING id",
        "DELETE FROM v WHERE id = 1 RETURNING id",
        "MERGE INTO v USING (VALUES(1)) AS s(id) ON v.id = s.id WHEN MATCHED THEN UPDATE SET value = 2 WHEN NOT MATCHED THEN INSERT(id) VALUES(s.id) RETURNING v.id",
    ] {
        let plan = plan(sql);
        let observed = allocation_counter::measure(|| { assert_eq!(check(&catalog, &plan).unwrap(), RoleReference::from("reader")); });
        assert!(observed.bytes_total < 64 << 10, "{sql}: {observed:?}");
    }
    assert_eq!(catalog.borrow_calls.get(), 4);
}

fn named(name: &str, control: &ProductionControl<'_>) -> Result<Produced<RoleReference>, SQLError> {
    let (name, memory) = control.copy_text(name)?.into_parts();
    Ok(control.finish(RoleReference::Named(name), memory)?)
}
fn controlled(
    catalog: &Catalog,
    plan: &UnifiedPlan,
    control: &ProductionControl<'_>,
) -> Result<Produced<RoleReference>, SQLError> {
    let UnifiedPlan::Command(command) = plan else {
        panic!("command")
    };
    match &**command {
        CommandPlan::Insert(statement) => ensure_insert_with_control(catalog, statement, control),
        CommandPlan::Update(statement) => ensure_update_with_control(catalog, statement, control),
        CommandPlan::Delete(statement) => ensure_delete_with_control(catalog, statement, control),
        CommandPlan::Merge(statement) => ensure_merge_with_control(catalog, statement, control),
        _ => panic!("mutation"),
    }
}
const MUTATIONS: [&str; 4] = [
    "INSERT INTO v(id) VALUES(1) ON CONFLICT(id) DO UPDATE SET value = excluded.id WHERE v.id = 1 RETURNING id",
    "UPDATE v SET value = 2 WHERE id = 1 RETURNING id",
    "DELETE FROM v WHERE id = 1 RETURNING id",
    "MERGE INTO v USING (VALUES(1)) AS s(id) ON v.id = s.id WHEN MATCHED THEN UPDATE SET value = 2 WHEN NOT MATCHED THEN INSERT(id) VALUES(s.id) RETURNING v.id",
];

#[test]
fn view_authorization_admits_subjects_and_scratch_and_retires_partial_results() {
    let catalog = Catalog::new();
    for sql in MUTATIONS {
        let plan = plan(sql);
        let mut passed = 0;
        let mut rejected = 0;
        for available in [0, 1, 4, 8, 16, 32, 64, 128, 256, 1024, 4096] {
            let budget = MemoryBudget::new(1 << 20);
            let token = CancellationToken::new();
            let control = ProductionControl::new(&budget, &token, &token);
            let sibling = control.copy_text("independent").unwrap();
            let baseline = budget.used();
            let occupied = budget.reserve(budget.available() - available).unwrap();
            match controlled(&catalog, &plan, &control) {
                Ok(subject) => {
                    passed += 1;
                    assert_eq!(&*subject, &RoleReference::from("reader"));
                    assert_eq!(budget.used(), baseline + occupied.bytes() + "reader".len());
                    drop(subject);
                }
                Err(error) => {
                    rejected += 1;
                    assert_eq!(error.sqlstate(), Some("53200"), "{sql}: {error}");
                }
            }
            assert_eq!(budget.used(), baseline + occupied.bytes());
            assert_eq!(&**sibling, "independent");
            drop(occupied);
            drop(sibling);
            assert_eq!(budget.used(), 0);
        }
        assert!(passed > 0 && rejected > 1, "{sql}");
    }
}

#[test]
fn view_authorization_releases_selected_failures_both_cancellations_and_unwind() {
    let catalog = Catalog::new();
    for sql in MUTATIONS {
        let plan = plan(sql);
        let budget = MemoryBudget::new(64 << 10);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&budget, &original, &invoking);
        let sibling = budget.reserve(37).unwrap();
        for token in [&original, &invoking] {
            let calls = catalog.borrow_calls.get();
            token.cancel();
            assert_eq!(
                controlled(&catalog, &plan, &control)
                    .unwrap_err()
                    .sqlstate(),
                Some("57014")
            );
            assert_eq!(catalog.borrow_calls.get(), calls);
            assert_eq!(budget.used(), 37);
            token.reset();
        }
        catalog.fail_select.set(true);
        assert_eq!(
            controlled(&catalog, &plan, &control)
                .unwrap_err()
                .sqlstate(),
            Some("XX005")
        );
        assert_eq!(budget.used(), 37);
        catalog.fail_select.set(false);
        catalog.unwind.set(true);
        assert!(
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| controlled(
                &catalog, &plan, &control
            )))
            .is_err()
        );
        assert_eq!(budget.used(), 37);
        catalog.unwind.set(false);
        drop(controlled(&catalog, &plan, &control).unwrap());
        assert_eq!(budget.used(), 37);
        drop(sibling);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn view_authorization_rejects_large_subjects_before_copying_and_preserves_owner_selection() {
    let mut catalog = Catalog::new();
    for sql in MUTATIONS {
        let mut plan = plan(sql);
        let UnifiedPlan::Command(command) = &mut plan else {
            panic!("command")
        };
        let subject = match &mut **command {
            CommandPlan::Insert(statement) => &mut statement.target_privilege_subject,
            CommandPlan::Update(statement) => &mut statement.target_privilege_subject,
            CommandPlan::Delete(statement) => &mut statement.target_privilege_subject,
            CommandPlan::Merge(statement) => &mut statement.target_privilege_subject,
            _ => panic!("mutation"),
        };
        *subject = Some(RoleReference::Named("role".repeat(1 << 20)));
        let budget = MemoryBudget::new(16 << 10);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&budget, &token, &token);
        let observed = allocation_counter::measure(|| {
            assert_eq!(
                controlled(&catalog, &plan, &control)
                    .unwrap_err()
                    .sqlstate(),
                Some("53200")
            );
        });
        assert!(observed.bytes_total < 64 << 10, "{observed:?}");
        assert_eq!(budget.used(), 0);
    }
    catalog.view.options.clear();
    let budget = MemoryBudget::new(64 << 10);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let output = controlled(&catalog, &plan(MUTATIONS[1]), &control).unwrap();
    assert_eq!(&*output, &RoleReference::from("owner"));
    assert_eq!(budget.used(), "owner".len());
    drop(output);
    assert_eq!(budget.used(), 0);
}

#[test]
fn view_authorization_preserves_column_diagnostics_and_merge_privilege_order() {
    let catalog = Catalog::new();
    for (sql, state) in [
        ("INSERT INTO v(missing,id,id) VALUES(1,2,3)", "42701"),
        ("UPDATE v SET missing = 1, id = 2, id = 3", "42601"),
        ("INSERT INTO v(missing) VALUES(1)", "42703"),
        ("UPDATE v SET missing = 1", "42703"),
    ] {
        catalog.calls.borrow_mut().clear();
        assert_eq!(
            check(&catalog, &plan(sql)).unwrap_err().sqlstate(),
            Some(state),
            "{sql}"
        );
        assert!(catalog.calls.borrow().is_empty());
    }
    catalog.calls.borrow_mut().clear();
    check(&catalog, &plan("MERGE INTO v USING (VALUES(1)) AS s(id) ON v.id = s.id WHEN MATCHED AND s.id = 1 THEN UPDATE SET value = 2, id = 3 WHEN MATCHED THEN DELETE WHEN NOT MATCHED THEN INSERT(value,id) VALUES(2,s.id) RETURNING v.id")).unwrap();
    let calls = catalog.calls.borrow();
    assert_eq!(
        &calls[..5],
        [
            "Delete",
            "Insert:id",
            "Insert:value",
            "Update:id",
            "Update:value"
        ]
    );
    assert!(calls.last().unwrap().starts_with("select:"));
}

#[test]
fn nested_invoker_view_targets_select_the_statement_caller_after_enclosing_owner_checks() {
    let mut catalog = Catalog::new();
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    for invoker in [true, false] {
        catalog.view.definition.options[0].1 = invoker.to_string();
        let expected = RoleReference::from(if invoker { "reader" } else { "owner" });
        assert_eq!(
            &*next_layer_privilege_subject_with_control(&catalog, "v", &control).unwrap(),
            &expected
        );
        for sql in MUTATIONS {
            let mut plan = plan(sql);
            let UnifiedPlan::Command(command) = &mut plan else {
                panic!("command")
            };
            let subject = match &mut **command {
                CommandPlan::Insert(statement) => &mut statement.target_privilege_subject,
                CommandPlan::Update(statement) => &mut statement.target_privilege_subject,
                CommandPlan::Delete(statement) => &mut statement.target_privilege_subject,
                CommandPlan::Merge(statement) => &mut statement.target_privilege_subject,
                _ => panic!("mutation"),
            };
            *subject = Some("outer_view_owner".into());
            let output = controlled(&catalog, &plan, &control).unwrap();
            assert_eq!(&*output, &expected, "{sql}");
            assert_eq!(budget.used(), output.reserved_bytes());
            drop(output);
            assert_eq!(budget.used(), 0);
        }
    }
}
