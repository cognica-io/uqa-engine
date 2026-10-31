//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    ast::FunctionBinding, binding::production::ControlledBindingSnapshot, plan::CommandPlan,
    ColumnType, FunctionTypeResolver,
};
use std::sync::Mutex;
use uqa_core::{memory::MemoryBudget, CancellationToken};

struct Catalog<'a> {
    columns: Vec<ColumnDef>,
    binding: BindingSnapshot,
    events: Mutex<Vec<&'static str>>,
    on_bind: Option<&'a (dyn Fn() -> Result<(), SQLError> + Send + Sync)>,
}

impl Catalog<'_> {
    fn new() -> Self {
        let crate::Statement::CreateTable(table) =
            crate::compile("CREATE TABLE items(a integer,b text)")
                .unwrap()
                .remove(0)
        else {
            panic!("table")
        };
        Self {
            columns: table.columns,
            binding: crate::binding::fixture::empty_binding_context().into(),
            events: Mutex::new(Vec::with_capacity(64)),
            on_bind: None,
        }
    }
    fn context(&self) -> InferenceContext<'_> {
        InferenceContext {
            catalog: self,
            aggregates: self,
            routines: self,
            binding: self,
        }
    }
}
impl FunctionTypeResolver for Catalog<'_> {
    fn resolve_function_type(
        &self,
        _: &str,
        _: Option<&FunctionBinding>,
        _: &[Option<String>],
        _: &[Option<ColumnType>],
        _: bool,
    ) -> Result<Option<ColumnType>, SQLError> {
        Ok(None)
    }
}
impl RoutineResolution for Catalog<'_> {}
impl AggregateClassifier for Catalog<'_> {
    fn is_registered_aggregate(&self, _: &str) -> bool {
        false
    }
}
impl ConflictCatalog for Catalog<'_> {
    fn try_describe_table(&self, _: &str) -> Result<Option<Vec<ColumnDef>>, String> {
        panic!("inference borrows declarations")
    }
    fn enforced_keys(&self, _: &str) -> Result<Vec<EnforcedKey>, String> {
        panic!("inference does not select arbiters")
    }
    fn try_declared_table_constraints(&self, _: &str) -> Result<TableConstraintSet, String> {
        unreachable!()
    }
    fn with_column_metadata_with_control(
        &self,
        _: &str,
        control: &ProductionControl<'_>,
        read: &mut dyn FnMut(&[ColumnDef]) -> Result<(), SQLError>,
    ) -> Result<bool, SQLError> {
        control.check()?;
        self.events.lock().unwrap().push("columns");
        read(&self.columns)?;
        Ok(true)
    }
}
impl InferenceBindingScope for Catalog<'_> {
    fn binding_scope(&self) -> Result<BindingSnapshot, SQLError> {
        panic!("inference captures controlled scope")
    }
    fn binding_scope_with_control(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<ControlledBindingSnapshot, SQLError> {
        self.events.lock().unwrap().push("binding");
        if let Some(bind) = self.on_bind {
            bind()?;
        }
        self.binding.copy_with_control(control)
    }
}

fn insert(sql: &str) -> Box<InsertPlan> {
    let crate::plan::UnifiedPlan::Command(command) =
        crate::plan::UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0))
    else {
        panic!("command")
    };
    let CommandPlan::Insert(insert) = *command else {
        panic!("insert")
    };
    insert
}

#[test]
fn controlled_inference_retains_exact_private_command_storage_and_promotes_columns() {
    let catalog = Catalog::new();
    let source = insert("INSERT INTO items VALUES(1,'x') ON CONFLICT(a,(items.a::integer),lower(b)) WHERE items.a>0 DO NOTHING RETURNING a");
    let ordinary = prepare_inference_predicate(catalog.context(), &source, &[]).unwrap();
    let budget = MemoryBudget::new(16 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    catalog.events.lock().unwrap().clear();
    let sibling = control.copy_text("retained sibling").unwrap();
    let baseline = budget.used();
    let mut output = None;
    let measured = allocation_counter::measure(|| {
        output = Some(
            prepare_inference_predicate_with_control(catalog.context(), &source, &[], &control)
                .unwrap()
                .unwrap(),
        );
    });
    let output = output.unwrap();
    assert_eq!(
        output.reserved_bytes(),
        usize::try_from(measured.bytes_current).unwrap(),
        "{measured:?}"
    );
    assert_eq!(budget.used(), baseline + output.reserved_bytes());
    let CommandPlan::Insert(prepared) = &*output else {
        panic!("insert")
    };
    assert_eq!(
        serde_json::to_value(&**prepared).unwrap(),
        serde_json::to_value(ordinary.as_ref()).unwrap()
    );
    let conflict = prepared.on_conflict.as_ref().unwrap();
    assert_eq!(conflict.conflict_columns, ["a", "a"]);
    assert_eq!(conflict.expressions.len(), 1);
    assert_eq!(
        &*catalog.events.lock().unwrap(),
        &["columns", "binding", "binding", "binding"]
    );
    drop(output);
    assert_eq!(budget.used(), baseline);
    assert_eq!(&**sibling, "retained sibling");
    drop(sibling);
    assert_eq!(budget.used(), 0);
}

#[test]
fn controlled_inference_preserves_bypass_and_validation_callback_order() {
    let catalog = Catalog::new();
    let budget = MemoryBudget::new(16 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    for sql in [
        "INSERT INTO items VALUES(1,'x')",
        "INSERT INTO items VALUES(1,'x') ON CONFLICT(a) DO NOTHING",
    ] {
        assert!(prepare_inference_predicate_with_control(
            catalog.context(),
            &insert(sql),
            &[],
            &control
        )
        .unwrap()
        .is_none());
        assert!(catalog.events.lock().unwrap().is_empty());
        assert_eq!(budget.used(), 0);
    }
    for (predicate, state) in [
        ("(SELECT true)", "0A000"),
        ("sum(a)>0", "42803"),
        ("row_number() OVER()>0", "42P20"),
        ("missing>0", "42703"),
    ] {
        catalog.events.lock().unwrap().clear();
        let source = insert(&format!(
            "INSERT INTO items VALUES(1,'x') ON CONFLICT(a) WHERE {predicate} DO NOTHING"
        ));
        let error =
            prepare_inference_predicate_with_control(catalog.context(), &source, &[], &control)
                .unwrap_err();
        assert_eq!(error.sqlstate(), Some(state), "{predicate}: {error}");
        let expected = if state == "42703" {
            vec!["columns", "binding"]
        } else {
            vec!["columns"]
        };
        assert_eq!(*catalog.events.lock().unwrap(), expected);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn controlled_inference_releases_private_work_on_binding_failure_cancellation_and_unwind() {
    let source =
        insert("INSERT INTO items VALUES(1,'x') ON CONFLICT(lower(b)) WHERE a>0 DO NOTHING");
    for failure in 0..4 {
        let budget = MemoryBudget::new(16 << 20);
        let statement = CancellationToken::new();
        let session = CancellationToken::new();
        let control = ProductionControl::new(&budget, &statement, &session);
        let sibling = control.copy_text("sibling").unwrap();
        let baseline = budget.used();
        let fail = || match failure {
            0 => Err(SQLError::Internal("scope callback failed".into())),
            1 => {
                statement.cancel();
                Ok(())
            }
            2 => {
                session.cancel();
                Ok(())
            }
            _ => panic!("scope callback unwinds"),
        };
        let mut catalog = Catalog::new();
        catalog.on_bind = Some(&fail);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            prepare_inference_predicate_with_control(catalog.context(), &source, &[], &control)
        }));
        if failure == 3 {
            assert!(result.is_err());
        } else {
            let error = result.unwrap().unwrap_err();
            if failure == 0 {
                assert!(error.to_string().contains("scope callback failed"));
            } else {
                assert_eq!(error.sqlstate(), Some("57014"));
            }
        }
        assert_eq!(&*catalog.events.lock().unwrap(), &["columns", "binding"]);
        assert_eq!(budget.used(), baseline);
        assert_eq!(&**sibling, "sibling");
        catalog.on_bind = None;
        catalog.events.lock().unwrap().clear();
        let fresh = CancellationToken::new();
        let recovered = ProductionControl::new(&budget, &fresh, &fresh);
        drop(
            prepare_inference_predicate_with_control(catalog.context(), &source, &[], &recovered)
                .unwrap(),
        );
        assert_eq!(budget.used(), baseline);
        drop(sibling);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn controlled_inference_rejects_large_unobserved_command_fields_before_copying() {
    let catalog = Catalog::new();
    let mut source =
        insert("INSERT INTO items VALUES(1,'x') ON CONFLICT(lower(b)) WHERE a>0 DO NOTHING");
    source.rows[0][1] = Expr::Literal(Value::Str("x".repeat(4 << 20)));
    let budget = MemoryBudget::new(64 << 10);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let sibling = control.copy_text("sibling").unwrap();
    let baseline = budget.used();
    let measured = allocation_counter::measure(|| {
        let error =
            prepare_inference_predicate_with_control(catalog.context(), &source, &[], &control)
                .unwrap_err();
        assert_eq!(error.sqlstate(), Some("53200"));
    });
    assert!(measured.bytes_total < 32 << 10, "{measured:?}");
    assert_eq!(budget.used(), baseline);
    assert_eq!(&*catalog.events.lock().unwrap(), &["columns"]);
    drop(sibling);
    assert_eq!(budget.used(), 0);
}

#[test]
fn controlled_inference_retires_an_expression_buffer_after_all_columns_are_promoted() {
    let catalog = Catalog::new();
    let source = insert("INSERT INTO items VALUES(1,'x') ON CONFLICT((items.a::integer),(items.a::integer)) DO NOTHING");
    drop(prepare_inference_predicate(catalog.context(), &source, &[]).unwrap());
    catalog.events.lock().unwrap().clear();
    let budget = MemoryBudget::new(16 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let mut output = None;
    let measured = allocation_counter::measure(|| {
        output = Some(
            prepare_inference_predicate_with_control(catalog.context(), &source, &[], &control)
                .unwrap()
                .unwrap(),
        );
    });
    let output = output.unwrap();
    let CommandPlan::Insert(plan) = &*output else {
        panic!("insert")
    };
    let conflict = plan.on_conflict.as_ref().unwrap();
    assert_eq!(conflict.conflict_columns, ["a", "a"]);
    assert_eq!(conflict.expressions.capacity(), 0);
    assert_eq!(
        output.reserved_bytes(),
        usize::try_from(measured.bytes_current).unwrap(),
        "{measured:?}"
    );
    drop(output);
    assert_eq!(budget.used(), 0);
}
