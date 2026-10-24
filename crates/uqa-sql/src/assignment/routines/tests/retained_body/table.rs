//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::borrow::Cow;

fn table_call() -> (
    Arc<crate::routines::SQLUserFunction>,
    crate::ast::FunctionBinding,
) {
    let (mut function, mut binding) = bound_call();
    Arc::get_mut(&mut function).unwrap().def.returns = FunctionReturns::SetOf {
        type_name: "public.accepted_value".into(),
    };
    binding.invocation.as_mut().unwrap().return_type = Some("public.accepted_value".into());
    (function, binding)
}

#[test]
fn table_result_descriptions_bound_repeated_declaration_work() {
    let values = Context::new();
    let types = Types {
        values: &values,
        calls: Cell::new(0),
    };
    let (function, binding) = table_call();
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
    for index in 0..512 {
        let layout = saved
            .table_result_layout(&types, &function.def, environment)
            .unwrap();
        assert_eq!(layout.kind, SQLFunctionResultKind::Value);
        assert_eq!(layout.declared_type, values.domain.column_type());
        assert!(layout.columns.is_none());
        let input = if index % 17 == 0 {
            Value::Null
        } else {
            Value::Int(index)
        };
        assert_eq!(
            crate::assignment::routines::coerce_routine_value_from(
                &values,
                &input,
                "public.accepted_value",
                Some(&ColumnType::Integer),
            )
            .unwrap(),
            input
        );
    }
    assert_eq!(
        values.checks.get(),
        512,
        "each current domain check remains live"
    );
    assert_eq!(
        types.calls.get(),
        1,
        "one declaration for the fixed table call"
    );
    assert!(budget.used() > 0);
    let repeated = allocation_counter::measure(|| {
        for _ in 0..512 {
            assert!(matches!(
                saved
                    .table_result_layout(&types, &function.def, environment)
                    .unwrap(),
                Cow::Borrowed(_)
            ));
        }
    });
    assert_eq!(
        repeated.bytes_total, 0,
        "retained declarations must be borrowed"
    );
    drop(state);
    assert_eq!(budget.used(), 0);
}

#[test]
fn table_declarations_recheck_changed_scope_and_cancellation() {
    let values = Context::new();
    let types = Types {
        values: &values,
        calls: Cell::new(0),
    };
    let (function, binding) = table_call();
    let state = RoutineCallDescriptions::default();
    let initial = RoutineDescriptionScope::default();
    let current = RefCell::new(Some(initial.clone()));
    let scope = || current.borrow().clone();
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
    for _ in 0..2 {
        assert!(matches!(
            saved
                .table_result_layout(&types, &function.def, environment)
                .unwrap(),
            Cow::Borrowed(_)
        ));
    }
    assert_eq!(types.calls.get(), 1);
    for changed in [Some(RoutineDescriptionScope::default()), None] {
        *current.borrow_mut() = changed;
        assert!(matches!(
            saved
                .table_result_layout(&types, &function.def, environment)
                .unwrap(),
            Cow::Owned(_)
        ));
    }
    assert_eq!(types.calls.get(), 3);
    *current.borrow_mut() = Some(initial);
    token.cancel();
    assert_eq!(
        saved
            .table_result_layout(&types, &function.def, environment)
            .unwrap_err()
            .sqlstate(),
        Some("57014")
    );
    assert_eq!(types.calls.get(), 3);
    drop(state);
    assert_eq!(budget.used(), 0);
}

#[test]
fn table_declarations_decline_full_allowance_and_search_path_names() {
    let values = Context::new();
    let types = Types {
        values: &values,
        calls: Cell::new(0),
    };
    let (function, binding) = table_call();
    let state = RoutineCallDescriptions::default();
    let initial = RoutineDescriptionScope::default();
    let scope = || Some(initial.clone());
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
    let remaining = budget.reserve(budget.limit() - budget.used()).unwrap();
    for _ in 0..2 {
        assert!(matches!(
            saved
                .table_result_layout(&types, &function.def, environment)
                .unwrap(),
            Cow::Owned(_)
        ));
    }
    assert_eq!(types.calls.get(), 2);
    drop(remaining);
    let before = budget.used();
    let mut unqualified = function.def.clone();
    unqualified.returns = FunctionReturns::SetOf {
        type_name: "accepted_value".into(),
    };
    for _ in 0..2 {
        assert!(matches!(
            saved
                .table_result_layout(&types, &unqualified, environment)
                .unwrap(),
            Cow::Owned(_)
        ));
    }
    assert_eq!(types.calls.get(), 4);
    assert_eq!(budget.used(), before);
    for _ in 0..2 {
        assert!(matches!(
            saved
                .table_result_layout(&types, &function.def, environment)
                .unwrap(),
            Cow::Borrowed(_)
        ));
    }
    assert_eq!(types.calls.get(), 5);
    assert!(
        budget.used() > before,
        "retained layout must own a charged allocation"
    );
    drop(state);
    assert_eq!(budget.used(), 0);
}

struct ChangingTypes<'a> {
    inner: Types<'a>,
    scope: &'a RefCell<RoutineDescriptionScope>,
    fail: Cell<bool>,
}

impl RoutineTypeCatalog for ChangingTypes<'_> {
    fn try_describe_table(&self, name: &str) -> Result<Option<Vec<ColumnDef>>, String> {
        self.inner.try_describe_table(name)
    }
    fn resolve_catalog_column_type(&self, name: &str) -> Option<ColumnType> {
        self.inner.resolve_catalog_column_type(name)
    }
    fn resolve_catalog_column_type_name(&self, name: &str) -> Result<ColumnType, SQLError> {
        let ty = self.inner.resolve_catalog_column_type_name(name)?;
        if self.fail.get() {
            return Err(SQLError::Routine {
                sqlstate: "42704".into(),
                message: "type was removed".into(),
            });
        }
        *self.scope.borrow_mut() = RoutineDescriptionScope::default();
        Ok(ty)
    }
    fn resolve_catalog_user_type_by_oid(&self, oid: u32) -> Option<ColumnType> {
        self.inner.resolve_catalog_user_type_by_oid(oid)
    }
    fn require_type_usage(&self, ty: &ColumnType) -> Result<(), SQLError> {
        self.inner.require_type_usage(ty)
    }
    fn format_type(&self, ty: &ColumnType) -> Result<String, SQLError> {
        self.inner.format_type(ty)
    }
}

#[test]
fn failed_or_invalidated_table_declarations_are_never_retained() {
    let values = Context::new();
    let initial = RoutineDescriptionScope::default();
    let current = RefCell::new(initial.clone());
    let types = ChangingTypes {
        inner: Types {
            values: &values,
            calls: Cell::new(0),
        },
        scope: &current,
        fail: Cell::new(true),
    };
    let (function, binding) = table_call();
    let state = RoutineCallDescriptions::default();
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
    let before = budget.used();
    assert_eq!(
        saved
            .table_result_layout(&types, &function.def, environment)
            .unwrap_err()
            .sqlstate(),
        Some("42704")
    );
    types.fail.set(false);
    for _ in 0..2 {
        *current.borrow_mut() = initial.clone();
        assert!(matches!(
            saved
                .table_result_layout(&types, &function.def, environment)
                .unwrap(),
            Cow::Owned(_)
        ));
    }
    assert_eq!(types.inner.calls.get(), 3);
    assert_eq!(
        budget.used(),
        before,
        "callback-invalidated declarations cannot be published"
    );
    drop(state);
    assert_eq!(budget.used(), 0);
}
