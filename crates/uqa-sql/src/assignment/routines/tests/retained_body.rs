//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    ast::{ColumnDef, FunctionReturns},
    binding::statements::AnalyzedResult,
    routines::{
        declaration::RoutineTypeCatalog,
        invocation::descriptions::{
            RoutineCallDescriptions, RoutineConversionEnvironment, RoutineDescriptionScope,
        },
        result_check::SQLFunctionResultKind,
    },
};
use std::{cell::RefCell, rc::Rc, sync::Arc};
use uqa_core::memory::ProductionControl;

fn bound_call() -> (
    Arc<crate::routines::SQLUserFunction>,
    crate::ast::FunctionBinding,
) {
    let (mut function, binding) = super::retained_invocation::bound_call();
    let specialized = crate::routines::invocation::specialized_definition(
        &function.def,
        binding.invocation.as_ref().unwrap(),
    )
    .unwrap()
    .unwrap();
    Arc::get_mut(&mut function).unwrap().def = specialized;
    (function, binding)
}

struct Types<'a> {
    values: &'a Context,
    calls: Cell<usize>,
}

impl RoutineTypeCatalog for Types<'_> {
    fn try_describe_table(&self, _: &str) -> Result<Option<Vec<ColumnDef>>, String> {
        Ok(None)
    }
    fn resolve_catalog_column_type(&self, name: &str) -> Option<ColumnType> {
        matches!(name, "public.accepted_value" | "accepted_value")
            .then(|| self.values.domain.column_type())
            .or_else(|| ColumnType::from_sql_name(name).ok())
    }
    fn resolve_catalog_column_type_name(&self, name: &str) -> Result<ColumnType, SQLError> {
        self.calls.set(self.calls.get() + 1);
        self.resolve_catalog_column_type(name)
            .ok_or_else(|| SQLError::Internal(name.into()))
    }
    fn resolve_catalog_user_type_by_oid(&self, _: u32) -> Option<ColumnType> {
        None
    }
    fn require_type_usage(&self, _: &ColumnType) -> Result<(), SQLError> {
        Ok(())
    }
    fn format_type(&self, ty: &ColumnType) -> Result<String, SQLError> {
        Ok(ty.regtype_name())
    }
}

#[test]
fn body_parameter_descriptions_follow_exact_positions_and_current_scope() {
    let values = Context::new();
    let (function, binding) = bound_call();
    let state = RoutineCallDescriptions::default();
    let initial = RoutineDescriptionScope::default();
    let current = RefCell::new(initial.clone());
    let scope = || Some(current.borrow().clone());
    let budget = MemoryBudget::new(8192);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    state
        .remember(
            &binding,
            &function,
            &[None, None],
            initial.clone(),
            &control,
        )
        .unwrap();
    let saved = state.get(&binding, &initial).unwrap();
    let environment = RoutineConversionEnvironment {
        values: &values,
        scope: &scope,
        control,
    };
    for _ in 0..512 {
        assert_eq!(
            *saved
                .body_parameter_type(0, "public.accepted_value", environment)
                .unwrap(),
            values.domain.column_type()
        );
    }
    assert_eq!(values.resolutions.get(), 1);
    assert_eq!(
        *saved
            .body_parameter_type(0, "pg_catalog.int8", environment)
            .unwrap(),
        ColumnType::BigInteger
    );
    *current.borrow_mut() = RoutineDescriptionScope::default();
    for _ in 0..2 {
        saved
            .body_parameter_type(0, "public.accepted_value", environment)
            .unwrap();
    }
    assert_eq!(values.resolutions.get(), 3);
    token.cancel();
    assert_eq!(
        saved
            .body_parameter_type(0, "public.accepted_value", environment)
            .unwrap_err()
            .sqlstate(),
        Some("57014")
    );
    drop(state);
    assert_eq!(budget.used(), 0);
}

#[test]
fn body_results_reuse_descriptions_while_values_and_domain_checks_stay_live() {
    let values = Context::new();
    let types = Types {
        values: &values,
        calls: Cell::new(0),
    };
    let (mut function, mut binding) = bound_call();
    Arc::get_mut(&mut function).unwrap().def.returns = FunctionReturns::Scalar {
        type_name: "public.accepted_value".into(),
    };
    binding.invocation.as_mut().unwrap().return_type = Some("public.accepted_value".into());
    let state = RoutineCallDescriptions::default();
    let initial = RoutineDescriptionScope::default();
    let scope = || Some(initial.clone());
    let budget = MemoryBudget::new(16 * 1024);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    state
        .remember(
            &binding,
            &function,
            &[None, None],
            initial.clone(),
            &control,
        )
        .unwrap();
    let saved = state.get(&binding, &initial).unwrap();
    let environment = RoutineConversionEnvironment {
        values: &values,
        scope: &scope,
        control,
    };
    let result = AnalyzedResult::Rows(vec![Some(ColumnType::Integer)]);
    for index in 0..512 {
        let layout = saved
            .body_result_layout(&types, &function.def, Some(&result), environment)
            .unwrap();
        assert_eq!(layout.kind, SQLFunctionResultKind::Value);
        assert_eq!(
            saved
                .coerce_body_result(
                    &layout,
                    0,
                    &Value::Int(index),
                    Some(&ColumnType::Integer),
                    environment
                )
                .unwrap(),
            Value::Int(index)
        );
    }
    assert_eq!(types.calls.get(), 1);
    assert_eq!(values.resolutions.get(), 1);
    assert_eq!(values.checks.get(), 512);
    values.accept.set(false);
    let layout = saved
        .body_result_layout(&types, &function.def, Some(&result), environment)
        .unwrap();
    assert_eq!(
        saved
            .coerce_body_result(&layout, 0, &Value::Int(-1), None, environment)
            .unwrap_err()
            .sqlstate(),
        Some("23514")
    );
    assert!(budget.used() < 16 * 1024);
    drop(layout);
    drop(state);
    assert_eq!(budget.used(), 0);
}

#[test]
fn result_schema_changes_and_missing_results_keep_the_original_checks() {
    let values = Context::new();
    let types = Types {
        values: &values,
        calls: Cell::new(0),
    };
    let (function, binding) = bound_call();
    let state = RoutineCallDescriptions::default();
    let initial = RoutineDescriptionScope::default();
    let scope = || Some(initial.clone());
    let control = ProductionControl::uncontrolled();
    state
        .remember(
            &binding,
            &function,
            &[None, None],
            initial.clone(),
            &control,
        )
        .unwrap();
    let saved = state.get(&binding, &initial).unwrap();
    let environment = RoutineConversionEnvironment {
        values: &values,
        scope: &scope,
        control,
    };
    let result = AnalyzedResult::Rows(vec![Some(ColumnType::Integer)]);
    saved
        .body_result_layout(&types, &function.def, Some(&result), environment)
        .unwrap();
    let reused = allocation_counter::measure(|| {
        for _ in 0..512 {
            assert!(matches!(
                saved
                    .body_result_layout(&types, &function.def, Some(&result), environment)
                    .unwrap(),
                std::borrow::Cow::Borrowed(_)
            ));
        }
    });
    assert_eq!(
        reused.bytes_total, 0,
        "result metadata reuse must not copy parsed names or descriptions"
    );
    assert_eq!(types.calls.get(), 1);
    let invalid = AnalyzedResult::Rows(vec![Some(ColumnType::Text)]);
    assert_eq!(
        saved
            .body_result_layout(&types, &function.def, Some(&invalid), environment)
            .unwrap_err()
            .sqlstate(),
        Some("42P13")
    );
    assert_eq!(
        saved
            .body_result_layout(&types, &function.def, None, environment)
            .unwrap_err()
            .sqlstate(),
        Some("42P13")
    );
    saved
        .body_result_layout(&types, &function.def, Some(&result), environment)
        .unwrap();
    assert_eq!(types.calls.get(), 3);
}

#[test]
fn output_callbacks_invalidate_later_values_and_do_not_publish_old_metadata() {
    let values = Context::new();
    let types = Types {
        values: &values,
        calls: Cell::new(0),
    };
    let (mut function, mut binding) = bound_call();
    Arc::get_mut(&mut function).unwrap().def.returns = FunctionReturns::Scalar {
        type_name: "public.accepted_value".into(),
    };
    binding.invocation.as_mut().unwrap().return_type = Some("public.accepted_value".into());
    let initial = RoutineDescriptionScope::default();
    let current = Rc::new(RefCell::new(initial.clone()));
    let callback = Rc::clone(&current);
    *values.after_check.borrow_mut() = Some(Box::new(move || {
        *callback.borrow_mut() = RoutineDescriptionScope::default();
    }));
    let scope = || Some(current.borrow().clone());
    let state = RoutineCallDescriptions::default();
    let control = ProductionControl::uncontrolled();
    state
        .remember(
            &binding,
            &function,
            &[None, None],
            initial.clone(),
            &control,
        )
        .unwrap();
    let saved = state.get(&binding, &initial).unwrap();
    let environment = RoutineConversionEnvironment {
        values: &values,
        scope: &scope,
        control,
    };
    let result = AnalyzedResult::Rows(vec![Some(ColumnType::Integer)]);
    let layout = saved
        .body_result_layout(&types, &function.def, Some(&result), environment)
        .unwrap();
    for index in 0..2 {
        assert_eq!(
            saved
                .coerce_body_result(&layout, 0, &Value::Int(index), None, environment)
                .unwrap(),
            Value::Int(index)
        );
    }
    assert_eq!(values.resolutions.get(), 2);
    saved
        .body_result_layout(&types, &function.def, Some(&result), environment)
        .unwrap();
    assert_eq!(types.calls.get(), 2);
}

#[test]
fn body_metadata_declines_exhausted_allowance_and_unqualified_results() {
    let values = Context::new();
    let types = Types {
        values: &values,
        calls: Cell::new(0),
    };
    let (function, binding) = bound_call();
    let scope = RoutineDescriptionScope::default();
    let current = || Some(scope.clone());
    let budget = MemoryBudget::new(8192);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let state = RoutineCallDescriptions::default();
    state
        .remember(&binding, &function, &[None, None], scope.clone(), &control)
        .unwrap();
    let saved = state.get(&binding, &scope).unwrap();
    let environment = RoutineConversionEnvironment {
        values: &values,
        scope: &current,
        control,
    };
    let remaining = budget.reserve(budget.limit() - budget.used()).unwrap();
    let result = AnalyzedResult::Rows(vec![Some(ColumnType::Integer)]);
    for index in 0..2 {
        saved
            .body_parameter_type(0, "public.accepted_value", environment)
            .unwrap();
        let layout = saved
            .body_result_layout(&types, &function.def, Some(&result), environment)
            .unwrap();
        assert_eq!(
            saved
                .coerce_body_result(&layout, 0, &Value::Int(index), None, environment)
                .unwrap(),
            Value::Int(index)
        );
    }
    assert_eq!(
        types.calls.get(),
        2,
        "full allowance must decline optional metadata retention"
    );
    assert_eq!(
        values.resolutions.get(),
        4,
        "both parameters and returned values still resolve live"
    );
    drop(remaining);
    let mut unqualified = function.def.clone();
    unqualified.returns = FunctionReturns::Scalar {
        type_name: "accepted_value".into(),
    };
    for _ in 0..2 {
        saved
            .body_result_layout(&types, &unqualified, Some(&result), environment)
            .unwrap();
    }
    assert_eq!(
        types.calls.get(),
        4,
        "a name dependent on search_path is not retained"
    );
    saved
        .body_result_layout(&types, &function.def, Some(&result), environment)
        .unwrap();
    let held = budget.used();
    saved
        .body_result_layout(&types, &function.def, Some(&result), environment)
        .unwrap();
    assert_eq!(types.calls.get(), 5);
    assert_eq!(budget.used(), held);
    token.cancel();
    assert_eq!(
        saved
            .body_result_layout(&types, &function.def, Some(&result), environment)
            .unwrap_err()
            .sqlstate(),
        Some("57014")
    );
    drop(state);
    assert_eq!(budget.used(), 0);
}

#[test]
fn tuple_result_targets_resolve_once_per_attribute_and_keep_each_domain_check() {
    let values = Context::new();
    let types = Types {
        values: &values,
        calls: Cell::new(0),
    };
    let (mut function, mut binding) = bound_call();
    let definition = &mut Arc::get_mut(&mut function).unwrap().def;
    definition.returns = FunctionReturns::None;
    definition.body = crate::ast::FunctionBody::Source("SELECT 1, 2".into());
    for parameter in &mut definition.params {
        parameter.mode = crate::ast::FunctionParamMode::Out;
    }
    definition.params[1].type_name = "pg_catalog.int4".into();
    binding.argument_types.clear();
    let invocation = binding.invocation.as_mut().unwrap();
    invocation.argument_positions.clear();
    invocation.argument_targets.clear();
    invocation.argument_sources.clear();
    invocation.parameter_types[1] = "pg_catalog.int4".into();
    invocation.return_type = Some("pg_catalog.record".into());
    let state = RoutineCallDescriptions::default();
    let scope = RoutineDescriptionScope::default();
    let current = || Some(scope.clone());
    let budget = MemoryBudget::new(16 * 1024);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    state
        .remember(&binding, &function, &[], scope.clone(), &control)
        .unwrap();
    let saved = state.get(&binding, &scope).unwrap();
    let environment = RoutineConversionEnvironment {
        values: &values,
        scope: &current,
        control,
    };
    let result = AnalyzedResult::Rows(vec![Some(ColumnType::Integer); 2]);
    for row in 0..512 {
        let layout = saved
            .body_result_layout(&types, &function.def, Some(&result), environment)
            .unwrap();
        assert!(
            matches!(layout, std::borrow::Cow::Borrowed(_)),
            "the first published layout and later hits share the retained description"
        );
        assert_eq!(layout.kind, SQLFunctionResultKind::Tuple);
        for index in 0..2 {
            let value = Value::Int(row * 2 + index as i64);
            assert_eq!(
                saved
                    .coerce_body_result(
                        &layout,
                        index,
                        &value,
                        Some(&ColumnType::Integer),
                        environment
                    )
                    .unwrap(),
                value
            );
        }
    }
    assert_eq!(types.calls.get(), 2);
    assert_eq!(values.resolutions.get(), 2);
    assert_eq!(
        values.checks.get(),
        512,
        "only the domain attribute has a live domain check"
    );
    drop(state);
    assert_eq!(budget.used(), 0);
}
