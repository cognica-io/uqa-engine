//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::ast::{
    ColumnType, FunctionDispatch, FunctionOrderSyntax, GeneratedColumn, GeneratedColumnKind,
};
use uqa_core::{memory::MemoryBudget, CancellationToken};

fn call(name: &str, binding: Option<FunctionBinding>, args: Vec<Expr>) -> Expr {
    Expr::Func {
        name: name.into(),
        binding,
        args,
        distinct: false,
        order_by: Vec::new(),
        order_syntax: FunctionOrderSyntax::default(),
        filter: None,
    }
}

fn compare(expression: &Expr) {
    for position in 0..4 {
        let mut column = ColumnDef::nullable("x", ColumnType::Text);
        let mut constraints = TableConstraintSet::default();
        match position {
            0 => column.default = Some(expression.clone()),
            1 => column.check = Some(expression.clone()),
            2 => {
                column.generated = Some(GeneratedColumn {
                    kind: GeneratedColumnKind::Stored,
                    expression: Box::new(expression.clone()),
                    function_dependencies: Vec::new(),
                });
            }
            _ => constraints
                .checks
                .push(serde_json::from_value(serde_json::json!({"expr": expression})).unwrap()),
        }
        let columns = [column];
        let before = serde_json::to_string(&(&columns, &constraints)).unwrap();
        let expected = super::super::upgrade_legacy_schema_function_dispatches(
            &mut columns.clone(),
            &mut constraints.clone(),
        );
        let memory = MemoryBudget::new(0);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&memory, &token, &token);
        let mut actual = None;
        let allocations = allocation_counter::measure(|| {
            actual = Some(
                has_legacy_schema_function_dispatches_with_control(
                    &columns,
                    &constraints,
                    &control,
                )
                .unwrap(),
            );
        });
        assert_eq!(actual, Some(expected), "{before}");
        assert_eq!(allocations.bytes_total, 0, "{before}: {allocations:?}");
        assert_eq!(
            serde_json::to_string(&(&columns, &constraints)).unwrap(),
            before
        );
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn stored_dispatch_observation_shares_name_binding_and_array_migration_selection() {
    for name in [
        "ordinary",
        "__array_slices",
        "__between_symmetric",
        "__to_hex_int4",
        "__range_contains_int4range",
        "__range_unknown_int4range",
    ] {
        for builtin in [false, true] {
            for dispatch in [
                None,
                Some(FunctionDispatch::ArraySlices),
                Some(FunctionDispatch::ToHexInt4),
            ] {
                let mut binding = FunctionBinding::dispatched(FunctionDispatch::ToHexInt4);
                binding.builtin = builtin;
                binding.dispatch = dispatch;
                compare(&call(
                    name,
                    Some(binding),
                    vec![Expr::Literal(Value::Null); 3],
                ));
            }
        }
        compare(&call(name, None, vec![Expr::Literal(Value::Null); 3]));
    }
    for count in 0..20 {
        for nulls in [false, true] {
            let args = (0..count)
                .map(|_| Expr::Literal(if nulls { Value::Null } else { Value::Int(1) }))
                .collect();
            compare(&call(
                "array",
                Some(FunctionBinding::dispatched(FunctionDispatch::ArraySlices)),
                args,
            ));
        }
    }
}

#[test]
fn stored_dispatch_observation_shares_range_predicate_selection_without_expansion() {
    let between = Expr::Between {
        expr: Box::new(Expr::Column("original".repeat(8192).into())),
        low: Box::new(Expr::Literal(Value::Int(1))),
        high: Box::new(Expr::Literal(Value::Int(3))),
    };
    compare(&between);
    compare(&Expr::Not(Box::new(between)));
    for count in 0..5 {
        for builtin in [false, true] {
            for distinct in [false, true] {
                let mut binding = FunctionBinding::dispatched(FunctionDispatch::BetweenSymmetric);
                binding.builtin = builtin;
                let mut value = call(
                    "between",
                    Some(binding),
                    vec![Expr::Literal(Value::Int(1)); count],
                );
                let Expr::Func {
                    distinct: target, ..
                } = &mut value
                else {
                    unreachable!()
                };
                *target = distinct;
                compare(&value);
            }
        }
    }
}

#[test]
fn stored_dispatch_observation_checks_both_owners_without_mutating_declarations() {
    let mut column = ColumnDef::nullable("x", ColumnType::Text);
    column.default = Some(call(
        "__array_slices",
        None,
        vec![Expr::Literal(Value::Null); 3],
    ));
    let columns = [column];
    let constraints = TableConstraintSet::default();
    let before = serde_json::to_string(&columns).unwrap();
    for original_cancelled in [false, true] {
        let memory = MemoryBudget::new(0);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        if original_cancelled {
            original.cancel();
        } else {
            invoking.cancel();
        }
        let control = ProductionControl::new(&memory, &original, &invoking);
        assert_eq!(
            has_legacy_schema_function_dispatches_with_control(&columns, &constraints, &control)
                .unwrap_err()
                .sqlstate(),
            Some("57014")
        );
        assert_eq!(serde_json::to_string(&columns).unwrap(), before);
        assert_eq!(memory.peak(), 0);
    }
}
