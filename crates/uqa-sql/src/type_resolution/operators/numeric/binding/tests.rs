//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{ast::FunctionDispatch, ColumnType};
use uqa_core::{memory::MemoryBudget, CancellationToken};

// The test root preserves the same value-before-lease destruction order as the caller's expression owner.
struct Root {
    binding: FunctionBinding,
    memory: Option<MemoryReservation>,
}

impl Root {
    fn new(operator: NumericOperator, control: &ProductionControl<'_>) -> Self {
        let (name, memory) = control.copy_text(operator.symbol()).unwrap().into_parts();
        Self {
            binding: FunctionBinding {
                object_id: None,
                name,
                argument_types: Vec::new(),
                builtin: true,
                dispatch: Some(FunctionDispatch::NumericOperator(operator)),
                invocation: None,
                resolution_error: None,
            },
            memory,
        }
    }

    fn bytes(&self) -> usize {
        self.memory.as_ref().map_or(0, MemoryReservation::bytes)
    }

    fn unchanged(&self) {
        assert!(self.binding.argument_types.is_empty());
        assert!(self.binding.resolution_error.is_none());
    }
}

#[test]
fn selected_numeric_bindings_retain_only_completed_names_and_root_payloads() {
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    for operator in [
        NumericOperator::Plus,
        NumericOperator::Absolute,
        NumericOperator::Modulo,
        NumericOperator::Power,
        NumericOperator::SquareRoot,
        NumericOperator::CubeRoot,
    ] {
        let mut root = Root::new(operator, &control);
        let args = vec![ScalarExpr::Literal(Value::Int(1)); operator.arity()];
        let types = vec![Some(ColumnType::Integer); operator.arity()];
        let selected = super::super::numeric_operator_types(operator, &types).unwrap();
        let mut infer =
            |_: &ScalarExpr| Ok(Some(ColumnType::Integer.clone_with_control(&control)?));
        bind_call_in_place_with_control(
            operator,
            &mut root.binding,
            &args,
            &mut root.memory,
            &mut infer,
            &control,
        )
        .unwrap();
        assert_eq!(
            root.binding.argument_types,
            selected
                .arguments
                .iter()
                .map(ColumnType::sql_name)
                .collect::<Vec<_>>()
        );
        assert!(root.binding.resolution_error.is_none());
        assert_eq!(
            root.bytes(),
            root.binding.name.capacity()
                + root.binding.argument_types.capacity() * size_of::<String>()
                + root
                    .binding
                    .argument_types
                    .iter()
                    .map(String::capacity)
                    .sum::<usize>()
        );
        assert_eq!(budget.used(), root.bytes());
        drop(root);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn semantic_selection_errors_retain_their_original_diagnostic_and_box() {
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let mut root = Root::new(NumericOperator::Plus, &control);
    let expected =
        super::super::numeric_operator_types(NumericOperator::Plus, &[Some(ColumnType::Boolean)])
            .unwrap_err();
    let mut infer = |_: &ScalarExpr| Ok(Some(ColumnType::Boolean.clone_with_control(&control)?));
    bind_call_in_place_with_control(
        NumericOperator::Plus,
        &mut root.binding,
        &[ScalarExpr::Literal(Value::Bool(true))],
        &mut root.memory,
        &mut infer,
        &control,
    )
    .unwrap();
    let Some(FunctionResolutionError::Operator(error)) = &root.binding.resolution_error else {
        panic!("operator error must be retained");
    };
    assert_eq!(Some(error.sqlstate.as_str()), expected.sqlstate());
    assert_eq!(error.message, expected.to_string());
    assert_eq!(
        root.bytes(),
        root.binding.name.capacity()
            + size_of::<OperatorResolutionError>()
            + error.sqlstate.capacity()
            + error.message.capacity()
    );
    assert_eq!(budget.used(), root.bytes());
    drop(root);
    assert_eq!(budget.used(), 0);
}

#[test]
fn unknown_constants_and_parameters_resolve_but_unresolved_expressions_defer() {
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    for (expression, deferred) in [
        (ScalarExpr::Literal(Value::Str("1".into())), false),
        (ScalarExpr::Literal(Value::Null), false),
        (ScalarExpr::Param(1), false),
        (ScalarExpr::Column("missing".into()), true),
        (ScalarExpr::Position(0), true),
    ] {
        let mut root = Root::new(NumericOperator::SquareRoot, &control);
        let before = root.bytes();
        let mut infer = |_: &ScalarExpr| Ok(None);
        bind_call_in_place_with_control(
            NumericOperator::SquareRoot,
            &mut root.binding,
            &[expression],
            &mut root.memory,
            &mut infer,
            &control,
        )
        .unwrap();
        if deferred {
            root.unchanged();
            assert_eq!(root.bytes(), before);
        } else {
            assert_eq!(root.binding.argument_types, ["double precision"]);
            assert!(root.binding.resolution_error.is_none());
        }
        assert_eq!(budget.used(), root.bytes());
        drop(root);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn inference_errors_defer_semantic_failures_and_propagate_resource_failures() {
    for sqlstate in ["42703", "42804", "53200", "57014"] {
        let budget = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&budget, &token, &token);
        let mut root = Root::new(NumericOperator::Modulo, &control);
        let before = root.bytes();
        let mut calls = 0;
        let mut infer = |_: &ScalarExpr| {
            calls += 1;
            if calls == 1 {
                return Ok(Some(ColumnType::Integer.clone_with_control(&control)?));
            }
            Err(SQLError::Routine {
                sqlstate: sqlstate.into(),
                message: "inference".into(),
            })
        };
        let result = bind_call_in_place_with_control(
            NumericOperator::Modulo,
            &mut root.binding,
            &[
                ScalarExpr::Literal(Value::Int(1)),
                ScalarExpr::Literal(Value::Int(2)),
            ],
            &mut root.memory,
            &mut infer,
            &control,
        );
        if matches!(sqlstate, "53200" | "57014") {
            assert_eq!(result.unwrap_err().sqlstate(), Some(sqlstate));
        } else {
            result.unwrap();
        }
        assert_eq!(calls, 2);
        root.unchanged();
        assert_eq!(root.bytes(), before);
        assert_eq!(budget.used(), before);
        drop(root);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn failed_name_and_error_construction_preserve_the_incoming_root_lease() {
    for ty in [ColumnType::Integer, ColumnType::Boolean] {
        let mut failed = false;
        let mut succeeded = false;
        for limit in [8, 32, 64, 128, 256, 512, 1024, 2048, 4096, 65536] {
            let budget = MemoryBudget::new(limit);
            let token = CancellationToken::new();
            let control = ProductionControl::new(&budget, &token, &token);
            let mut root = Root::new(NumericOperator::Plus, &control);
            let before = root.bytes();
            let mut infer = |_: &ScalarExpr| Ok(Some(ty.clone_with_control(&control)?));
            match bind_call_in_place_with_control(
                NumericOperator::Plus,
                &mut root.binding,
                &[ScalarExpr::Literal(Value::Int(1))],
                &mut root.memory,
                &mut infer,
                &control,
            ) {
                Ok(()) => succeeded = true,
                Err(error) => {
                    assert_eq!(error.sqlstate(), Some("53200"));
                    root.unchanged();
                    assert_eq!(root.bytes(), before);
                    failed = true;
                }
            }
            assert_eq!(budget.used(), root.bytes());
            drop(root);
            assert_eq!(budget.used(), 0);
        }
        assert!(failed && succeeded);
    }
}

#[test]
fn both_cancellation_scopes_leave_root_ownership_intact() {
    for original_cancelled in [true, false] {
        let budget = MemoryBudget::new(1 << 20);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&budget, &original, &invoking);
        let mut root = Root::new(NumericOperator::Plus, &control);
        let before = root.bytes();
        let mut infer = |_: &ScalarExpr| {
            if original_cancelled {
                original.cancel();
            } else {
                invoking.cancel();
            }
            Err(SQLError::UnknownColumn("missing".into()))
        };
        let error = bind_call_in_place_with_control(
            NumericOperator::Plus,
            &mut root.binding,
            &[ScalarExpr::Literal(Value::Int(1))],
            &mut root.memory,
            &mut infer,
            &control,
        )
        .unwrap_err();
        assert_eq!(error.sqlstate(), Some("57014"));
        root.unchanged();
        assert_eq!(root.bytes(), before);
        assert_eq!(budget.used(), before);
        drop(root);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn ordinary_numeric_binding_preserves_custom_resolver_error_deferral() {
    struct Resolver;
    impl crate::FunctionTypeResolver for Resolver {
        fn resolve_type_name(&self, _: &str) -> Result<Option<ColumnType>, SQLError> {
            Err(SQLError::Routine {
                sqlstate: "53200".into(),
                message: "custom resolver".into(),
            })
        }
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
    let mut root = Root::new(NumericOperator::Plus, &ProductionControl::uncontrolled());
    super::super::bind_call(
        NumericOperator::Plus,
        &mut root.binding,
        &[ScalarExpr::Cast {
            expr: Box::new(ScalarExpr::Literal(Value::Int(1))),
            ty: "custom_numeric_domain".into(),
        }],
        &crate::schema::ColumnTypeSchema::new(&[]),
        &[],
        Some(&Resolver),
    );
    root.unchanged();
}

#[test]
fn retained_error_box_admission_releases_prepared_strings_on_failure() {
    let input = SQLError::Routine {
        sqlstate: "42883".into(),
        message: "invalid".into(),
    };
    let token = CancellationToken::new();
    let probe = MemoryBudget::new(1024);
    let result = retained_error(&input, &ProductionControl::new(&probe, &token, &token)).unwrap();
    let FunctionResolutionError::Operator(error) = &*result else {
        unreachable!()
    };
    let strings = error.sqlstate.capacity() + error.message.capacity();
    assert_eq!(
        result.reserved_bytes(),
        strings + size_of::<OperatorResolutionError>()
    );
    drop(result);
    assert_eq!(probe.used(), 0);
    let budget = MemoryBudget::new(strings);
    let error =
        retained_error(&input, &ProductionControl::new(&budget, &token, &token)).unwrap_err();
    assert_eq!(error.sqlstate(), Some("53200"));
    assert_eq!(budget.used(), 0);
}
