//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::Value;
use uqa_sql::ast::{Expr, GeneratedColumn};

fn generated(expression: Expr) -> GeneratedColumn {
    GeneratedColumn {
        kind: GeneratedColumnKind::Virtual,
        expression: Box::new(expression),
        function_dependencies: Vec::new(),
    }
}

#[test]
fn generated_preparation_retains_lowering_lease_through_evaluation() {
    let budget = MemoryBudget::new(1 << 16);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = GeneratedControl {
        budget: &budget,
        original: &original,
        invoking: &invoking,
    };
    let generated = generated(Expr::Literal(Value::Str("literal".repeat(100))));
    let expression =
        prepare_generated_column_with_control(&crate::RowSchema::default(), &generated, &control)
            .unwrap();
    let retained = budget.used();
    assert!(retained >= 700);
    let row = Document::new();
    assert_eq!(
        evaluate_generated_expression(&expression.scalar, &row).unwrap(),
        Value::Str("literal".repeat(100))
    );
    assert_eq!(budget.used(), retained);
    drop(expression);
    assert_eq!(budget.used(), 0);
}

#[test]
fn generated_lowering_preserves_typed_quota_and_both_cancellation_failures() {
    let budget = MemoryBudget::new(64);
    let generated = generated(Expr::Literal(Value::Str("literal".repeat(100))));
    for cancelled in [None, Some(true), Some(false)] {
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        match cancelled {
            Some(true) => original.cancel(),
            Some(false) => invoking.cancel(),
            None => {}
        }
        let control = GeneratedControl {
            budget: &budget,
            original: &original,
            invoking: &invoking,
        };
        let result = prepare_generated_column_with_control(
            &crate::RowSchema::default(),
            &generated,
            &control,
        );
        match cancelled {
            None => assert!(
                matches!(result, Err(SQLError::Routine { sqlstate, .. }) if sqlstate == "53200")
            ),
            Some(_) => assert!(matches!(result, Err(SQLError::Cancelled(_)))),
        }
        assert_eq!(budget.used(), 0);
    }
}

fn declared_columns() -> Vec<ColumnDef> {
    let uqa_sql::ast::Statement::CreateTable(table) = uqa_sql::compile(
        "CREATE TABLE t (v smallint, label varchar(12), kind regtype GENERATED ALWAYS AS (pg_typeof(v)) VIRTUAL)",
    ).unwrap().remove(0) else { panic!("table declaration"); };
    table.columns
}

#[test]
fn generated_reads_borrow_retained_column_types_without_changing_results() {
    let columns = declared_columns();
    let schema = ColumnTypeSchema::new(&columns);
    let physical = crate::RowSchema::with_types(
        columns.iter().map(|column| column.name.clone()).collect(),
        columns
            .iter()
            .map(|column| Some(column.ty.clone()))
            .collect(),
    );
    let definition = columns[2].generated.as_ref().unwrap();
    assert_eq!(
        prepare_generated_column(&schema, definition).unwrap(),
        prepare_generated_column(&physical, definition).unwrap()
    );
    let mut document = Document::from([
        ("v".into(), Value::Int(3)),
        ("label".into(), Value::Str("text".into())),
    ]);
    materialize_missing_generated_columns(&columns, &mut document).unwrap();
    assert_eq!(document["kind"], Value::Str("smallint".into()));
    assert_eq!(document["v"], Value::Int(3));
}

struct CancelOnLookup<'a> {
    schema: ColumnTypeSchema<'a>,
    cancellation: &'a CancellationToken,
}

impl ScalarTypeSchema for CancelOnLookup<'_> {
    fn has_unqualified_column(&self, name: &str) -> bool {
        self.schema.has_unqualified_column(name)
    }
    fn column_is_ambiguous(&self, name: &str) -> bool {
        self.schema.column_is_ambiguous(name)
    }
    fn type_of(&self, name: &str) -> Option<&uqa_sql::ast::ColumnType> {
        self.cancellation.cancel();
        self.schema.type_of(name)
    }
    fn column_type(&self, position: usize) -> Option<&uqa_sql::ast::ColumnType> {
        self.schema.column_type(position)
    }
    fn internal_type(
        &self,
        _: uqa_sql::ast::InternalColumnRef,
    ) -> Option<&uqa_sql::ast::ColumnType> {
        None
    }
    fn has_qualifier(&self, _: &str) -> bool {
        false
    }
    fn has_qualified_column(&self, _: &str, _: &str) -> bool {
        false
    }
    fn qualified_column_is_ambiguous(&self, _: &str, _: &str) -> bool {
        false
    }
    fn qualified_type(&self, _: &str, _: &str) -> Option<&uqa_sql::ast::ColumnType> {
        None
    }
    fn columns_are_open(&self, _: Option<&str>) -> bool {
        false
    }
    fn physical_schema(&self) -> Option<&crate::RowSchema> {
        None
    }
}

#[test]
fn borrowed_schema_binding_preserves_original_and_invoking_cancellation() {
    let columns = declared_columns();
    let definition = columns[2].generated.as_ref().unwrap();
    for cancel_original in [true, false] {
        let budget = MemoryBudget::new(1 << 16);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let token = if cancel_original {
            &original
        } else {
            &invoking
        };
        let schema = CancelOnLookup {
            schema: ColumnTypeSchema::new(&columns),
            cancellation: token,
        };
        let control = GeneratedControl {
            budget: &budget,
            original: &original,
            invoking: &invoking,
        };
        assert!(matches!(
            prepare_generated_column_with_control(&schema, definition, &control),
            Err(SQLError::Cancelled(_))
        ));
        assert!(token.is_cancelled());
        assert!(budget.peak() > 0);
        assert_eq!(budget.used(), 0);
    }
    assert_eq!(columns[0].ty, uqa_sql::ast::ColumnType::SmallInteger);
}

#[test]
fn generated_preparation_retains_binder_allocations_and_rejects_partial_quota() {
    let columns = declared_columns();
    let schema = ColumnTypeSchema::new(&columns);
    let definition = columns[2].generated.as_ref().unwrap();
    let token = CancellationToken::new();
    let budget = MemoryBudget::new(1 << 16);
    let lowered = uqa_sql::plan::ExpressionPlan::lower_column_budgeted(
        &definition.expression,
        &budget,
        &token,
        &token,
    )
    .unwrap();
    let initial = budget.used();
    drop(lowered);
    let prepared = prepare_generated_column_with_control(
        &schema,
        definition,
        &GeneratedControl {
            budget: &budget,
            original: &token,
            invoking: &token,
        },
    )
    .unwrap();
    assert!(budget.used() > initial);
    assert_eq!(
        evaluate_generated_expression(&prepared.scalar, &Document::new()).unwrap(),
        Value::Str("smallint".into())
    );
    drop(prepared);
    assert_eq!(budget.used(), 0);
    let limited = MemoryBudget::new(initial);
    let error = prepare_generated_column_with_control(
        &schema,
        definition,
        &GeneratedControl {
            budget: &limited,
            original: &token,
            invoking: &token,
        },
    )
    .err()
    .expect("lowering succeeds but binding needs additional allocations");
    assert_eq!(error.sqlstate(), Some("53200"));
    assert_eq!(limited.used(), 0);
}

fn text_generated_columns() -> Vec<ColumnDef> {
    let uqa_sql::Statement::CreateTable(table) = uqa_sql::compile(
        "CREATE TABLE t (first TEXT GENERATED ALWAYS AS (repeat('x', 32768)) VIRTUAL, second TEXT GENERATED ALWAYS AS (repeat('y', 32768)) VIRTUAL)",
    ).unwrap().remove(0) else { panic!("table declaration"); };
    table.columns
}

#[test]
fn generated_document_results_keep_their_allowance_after_preparation_drops() {
    let columns = text_generated_columns();
    let budget = MemoryBudget::new(256 << 10);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = GeneratedControl {
        budget: &budget,
        original: &original,
        invoking: &invoking,
    };
    let mut memory = budget.empty_reservation();
    let mut document = Document::new();
    materialize_missing_generated_columns_with_control(
        &columns,
        &mut document,
        &mut memory,
        &control,
    )
    .unwrap();
    assert_eq!(document["first"], Value::Str("x".repeat(32768)));
    assert_eq!(document["second"], Value::Str("y".repeat(32768)));
    assert!(memory.bytes() >= 65536);
    assert_eq!(budget.used(), memory.bytes());
    for token in [&original, &invoking] {
        token.cancel();
        let used = budget.used();
        let error = materialize_missing_generated_columns_with_control(
            &columns,
            &mut document,
            &mut memory,
            &control,
        )
        .unwrap_err();
        assert_eq!(error.sqlstate(), Some("57014"));
        assert_eq!(budget.used(), used);
        assert_eq!(document.len(), 2);
        token.reset();
    }
    drop(document);
    drop(memory);
    assert_eq!(budget.used(), 0);
}

#[test]
fn later_generated_quota_failure_keeps_already_installed_values_charged() {
    let columns = text_generated_columns();
    let budget = MemoryBudget::new(80 << 10);
    let token = CancellationToken::new();
    let control = GeneratedControl {
        budget: &budget,
        original: &token,
        invoking: &token,
    };
    let mut memory = budget.empty_reservation();
    let mut document = Document::new();
    let error = materialize_missing_generated_columns_with_control(
        &columns,
        &mut document,
        &mut memory,
        &control,
    )
    .unwrap_err();
    assert_eq!(error.sqlstate(), Some("53200"));
    assert_eq!(document["first"], Value::Str("x".repeat(32768)));
    assert!(!document.contains_key("second"));
    assert!(memory.bytes() >= 32768);
    assert_eq!(budget.used(), memory.bytes());
    drop(document);
    drop(memory);
    assert_eq!(budget.used(), 0);
}

#[test]
fn generated_completion_preserves_present_nulls_without_constructing_replacements() {
    let columns = text_generated_columns();
    let budget = MemoryBudget::new(0);
    let token = CancellationToken::new();
    let control = GeneratedControl {
        budget: &budget,
        original: &token,
        invoking: &token,
    };
    let mut memory = budget.empty_reservation();
    let mut document = Document::from([
        ("first".into(), Value::Null),
        ("second".into(), Value::Null),
    ]);
    materialize_missing_generated_columns_with_control(
        &columns,
        &mut document,
        &mut memory,
        &control,
    )
    .unwrap();
    assert_eq!(document["first"], Value::Null);
    assert_eq!(document["second"], Value::Null);
    assert_eq!(budget.used(), 0);
}
