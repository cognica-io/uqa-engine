//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::super::test_support::Runtime;
use super::*;
use crate::query::table_functions::TableFunctionCall;
use crate::{OwnedPhysicalRow, PhysicalRow, RowSchema};
use std::cell::Cell;
use std::sync::atomic::Ordering;
use uqa_core::Value;
use uqa_sql::{ast::FunctionBinding, ColumnType};

fn source() -> SourcePlan {
    SourcePlan::FunctionGroup {
        functions: ["two_payload", "one_payload"]
            .into_iter()
            .map(|name| TableFunctionPlan {
                name: name.into(),
                binding: Some(FunctionBinding::builtin_by_name(name, None)),
                output_name: name.into(),
                relations: None,
                args: vec![uqa_sql::ScalarExpr::Literal(Value::Str(
                    "x".repeat(64 * 1024),
                ))],
                column_aliases: vec![],
                column_types: vec![],
            })
            .collect(),
        alias: Some("zipped".into()),
        column_aliases: vec![
            "a".into(),
            "a_length".into(),
            "b".into(),
            "b_length".into(),
            "n".into(),
        ],
        ordinality: true,
    }
}

fn outer(value: i64) -> OwnedPhysicalRow {
    OwnedPhysicalRow::new(
        RowSchema::with_types(vec!["outer".into()], vec![Some(ColumnType::BigInteger)]),
        PhysicalRow::from_values(vec![Value::Int(value)]),
    )
}

fn initialize<'a>(
    source: &FunctionSource<'_>,
    runtime: &Arc<dyn SetFunctionRuntime + 'a>,
    outer: &OwnedPhysicalRow,
) -> InitializedFunctionSource<'a> {
    let call = TableFunctionCall {
        name: source.name,
        binding: source.binding,
        output_name: source.output_name,
        relations: None,
        args: source.args,
        function_states: None,
        routine_descriptions: None,
        alias: None,
        column_aliases: &[],
        ordinality: false,
        column_types: &[],
    };
    let call = Arc::new(PreparedFunctionCall::new(&call, None));
    let columns = vec!["input".into(), "payload_length".into()];
    InitializedFunctionSource {
        schema: RowSchema::with_types(columns.clone(), vec![Some(ColumnType::BigInteger); 2]),
        columns,
        rows: deferred_function_rows(
            Arc::clone(&call),
            Arc::clone(runtime),
            &[],
            Some(outer.clone()),
            2,
        ),
        prepared: Some(call),
    }
}

#[test]
fn correlated_groups_prepare_once_and_keep_live_values_padding_and_ordinality() {
    let observer = Arc::new(Runtime::default());
    *observer.scope.lock().unwrap() = Some(RoutineDescriptionScope::default());
    let runtime: Arc<dyn SetFunctionRuntime> = observer.clone();
    let group = PreparedFunctionGroup::default();
    let source = source();
    let preparations = Cell::new(0);
    for value in 0..512 {
        let outer = outer(value);
        let operator = group
            .build(&source, &[], false, &runtime, &outer, |source| {
                assert_eq!(observer.calls.load(Ordering::Relaxed), 0);
                preparations.set(preparations.get() + 1);
                Ok(initialize(source, &runtime, &outer))
            })
            .unwrap();
        assert_eq!(preparations.get(), 2);
        assert_eq!(observer.calls.load(Ordering::Relaxed), value as usize * 2);
        let rows = crate::physical::demand::OperatorRows::open(operator)
            .unwrap()
            .map(|row| {
                let row = row.unwrap();
                (0..5)
                    .map(|position| row.physical_value_at(position).unwrap().clone())
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        assert_eq!(
            rows,
            vec![
                vec![
                    Value::Int(value),
                    Value::Int(65536),
                    Value::Int(value),
                    Value::Int(65536),
                    Value::Int(1)
                ],
                vec![
                    Value::Int(value + 1),
                    Value::Int(65536),
                    Value::Null,
                    Value::Null,
                    Value::Int(2)
                ],
            ]
        );
    }
    assert_eq!(observer.calls.load(Ordering::Relaxed), 1024);
}

#[test]
fn correlated_group_descriptions_decline_changed_scope_schema_and_mutable_hooks() {
    let observer = Arc::new(Runtime::default());
    *observer.scope.lock().unwrap() = Some(RoutineDescriptionScope::default());
    let runtime: Arc<dyn SetFunctionRuntime> = observer.clone();
    let group = PreparedFunctionGroup::default();
    let source = source();
    let preparations = Cell::new(0);
    let build = |outer: &OwnedPhysicalRow| {
        drop(
            group
                .build(&source, &[], false, &runtime, outer, |source| {
                    preparations.set(preparations.get() + 1);
                    Ok(initialize(source, &runtime, outer))
                })
                .unwrap(),
        );
    };
    let mut row = outer(10);
    build(&row);
    build(&outer(20));
    assert_eq!(preparations.get(), 2);
    row.schema = RowSchema::with_types(vec!["changed".into()], vec![Some(ColumnType::Integer)]);
    build(&row);
    assert_eq!(preparations.get(), 4);
    *observer.scope.lock().unwrap() = Some(RoutineDescriptionScope::default());
    build(&outer(30));
    assert_eq!(preparations.get(), 6);
    *observer.scope.lock().unwrap() = None;
    build(&outer(40));
    build(&outer(50));
    assert_eq!(preparations.get(), 10);
    assert_eq!(observer.calls.load(Ordering::Relaxed), 0);
}

#[test]
fn correlated_group_initialization_failure_precedes_every_member_effect() {
    let observer = Arc::new(Runtime::default());
    *observer.scope.lock().unwrap() = Some(RoutineDescriptionScope::default());
    let runtime: Arc<dyn SetFunctionRuntime> = observer.clone();
    let group = PreparedFunctionGroup::default();
    let source = source();
    let row = outer(1);
    let result = group.build(&source, &[], false, &runtime, &row, |source| {
        if source.name == "one_payload" {
            return Err(SQLError::Internal("second member rejected".into()));
        }
        Ok(initialize(source, &runtime, &row))
    });
    assert!(
        matches!(result, Err(SQLError::Internal(message)) if message == "second member rejected")
    );
    assert_eq!(observer.calls.load(Ordering::Relaxed), 0);
    assert!(group.description.get().is_none());
}

#[test]
fn correlated_group_scope_change_during_preparation_prevents_publication() {
    let observer = Arc::new(Runtime::default());
    *observer.scope.lock().unwrap() = Some(RoutineDescriptionScope::default());
    let runtime: Arc<dyn SetFunctionRuntime> = observer.clone();
    let group = PreparedFunctionGroup::default();
    let source = source();
    let row = outer(1);
    drop(
        group
            .build(&source, &[], false, &runtime, &row, |source| {
                if source.name == "one_payload" {
                    *observer.scope.lock().unwrap() = None;
                }
                Ok(initialize(source, &runtime, &row))
            })
            .unwrap(),
    );
    assert!(group.description.get().is_none());
    assert_eq!(observer.calls.load(Ordering::Relaxed), 0);
}
