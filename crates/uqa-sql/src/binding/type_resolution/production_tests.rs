//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::binding::{
    fixture::empty_binding_context, scoped_types::BindingTypeResolver, snapshot::BindingSnapshot,
};
use uqa_core::{
    memory::{MemoryBudget, Produced, ProductionControl},
    CancellationToken,
};

struct ControlledOnly;

fn denied<T>(control: &ProductionControl<'_>) -> Result<T, SQLError> {
    control.reserve(4096)?;
    panic!("the supplied allowance must reject this producer");
}

impl FunctionTypeResolver for ControlledOnly {
    fn format_type_name(&self, _: &ColumnType) -> Result<String, SQLError> {
        panic!("ordinary format callback")
    }
    fn resolve_type_name(&self, _: &str) -> Result<Option<ColumnType>, SQLError> {
        panic!("ordinary type callback")
    }
    fn resolve_function_type(
        &self,
        _: &str,
        _: Option<&crate::ast::FunctionBinding>,
        _: &[Option<String>],
        _: &[Option<ColumnType>],
        _: bool,
    ) -> Result<Option<ColumnType>, SQLError> {
        panic!("ordinary function callback")
    }
    fn resolve_function_overload(
        &self,
        _: &str,
        _: Option<&crate::ast::FunctionBinding>,
        _: &[Option<String>],
        _: &[Option<ColumnType>],
        _: bool,
    ) -> Result<Option<ResolvedFunctionOverload>, SQLError> {
        panic!("ordinary overload callback")
    }
    fn resolve_function_overload_with_builtins(
        &self,
        _: &str,
        _: Option<&crate::ast::FunctionBinding>,
        _: &[Option<String>],
        _: &[Option<ColumnType>],
        _: bool,
        _: &[crate::BuiltinFunctionOverload],
    ) -> Result<Option<ResolvedFunctionOverload>, SQLError> {
        panic!("ordinary builtin callback")
    }
    fn resolve_scalar_subquery_type(
        &self,
        _: crate::SubqueryId,
        _: &RowSchema,
        _: &[SQLParam],
    ) -> Result<Option<ColumnType>, SQLError> {
        panic!("ordinary subquery callback")
    }
    fn format_type_name_with_control(
        &self,
        _: &ColumnType,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<String>, SQLError> {
        denied(control)
    }
    fn resolve_type_name_with_control(
        &self,
        _: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Produced<ColumnType>>, SQLError> {
        denied(control)
    }
    fn resolve_function_type_with_control(
        &self,
        _: &str,
        _: Option<&crate::ast::FunctionBinding>,
        _: &[Option<String>],
        _: &[Option<ColumnType>],
        _: bool,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Produced<ColumnType>>, SQLError> {
        denied(control)
    }
    fn resolve_function_overload_with_control(
        &self,
        _: &str,
        _: Option<&crate::ast::FunctionBinding>,
        _: &[Option<String>],
        _: &[Option<ColumnType>],
        _: bool,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Produced<ResolvedFunctionOverload>>, SQLError> {
        denied(control)
    }
    fn resolve_function_overload_with_builtins_and_control(
        &self,
        _: &str,
        _: Option<&crate::ast::FunctionBinding>,
        _: &[Option<String>],
        _: &[Option<ColumnType>],
        _: bool,
        _: &[crate::BuiltinFunctionOverload],
        control: &ProductionControl<'_>,
    ) -> Result<Option<Produced<ResolvedFunctionOverload>>, SQLError> {
        denied(control)
    }
    fn resolve_scalar_subquery_type_with_control(
        &self,
        _: crate::SubqueryId,
        _: &RowSchema,
        _: &[SQLParam],
        control: &ProductionControl<'_>,
    ) -> Result<Option<Produced<ColumnType>>, SQLError> {
        denied(control)
    }
}
impl RoutineResolution for ControlledOnly {}

#[test]
fn schema_resolver_adapters_forward_the_original_producer_allowance() {
    let budget = MemoryBudget::new(128);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let sibling = budget.reserve(7).unwrap();
    let snapshot = BindingSnapshot::from(empty_binding_context());
    let binding = BindingTypeResolver {
        routines: &ControlledOnly,
        scope: &snapshot,
    };
    let query = QueryFunctionTypeResolver {
        routines: &ControlledOnly,
        scalar_subquery_schemas: None,
        defer_routine_namespace_errors: true,
    };
    for resolver in [
        &binding as &dyn FunctionTypeResolver,
        &query as &dyn FunctionTypeResolver,
    ] {
        assert_eq!(
            resolver
                .format_type_name_with_control(&ColumnType::Text, &control)
                .unwrap_err()
                .sqlstate(),
            Some("53200")
        );
        assert_eq!(
            resolver
                .resolve_type_name_with_control("selected", &control)
                .unwrap_err()
                .sqlstate(),
            Some("53200")
        );
        assert_eq!(
            resolver
                .resolve_function_type_with_control("selected", None, &[], &[], false, &control)
                .unwrap_err()
                .sqlstate(),
            Some("53200")
        );
        assert_eq!(
            resolver
                .resolve_function_overload_with_control("selected", None, &[], &[], false, &control)
                .unwrap_err()
                .sqlstate(),
            Some("53200")
        );
        assert_eq!(
            resolver
                .resolve_function_overload_with_builtins_and_control(
                    "selected",
                    None,
                    &[],
                    &[],
                    false,
                    &[],
                    &control
                )
                .unwrap_err()
                .sqlstate(),
            Some("53200")
        );
        assert_eq!(budget.used(), 7);
    }
    assert_eq!(
        query
            .resolve_scalar_subquery_type_with_control(0, &RowSchema::default(), &[], &control)
            .unwrap_err()
            .sqlstate(),
        Some("53200")
    );
    assert_eq!(budget.used(), 7);
    drop(sibling);
}

#[test]
fn cached_subquery_type_copies_only_the_selected_type_with_its_lease() {
    let name = "selected".repeat(16 * 1024);
    let resolver = QueryFunctionTypeResolver {
        routines: &ControlledOnly,
        scalar_subquery_schemas: Some(vec![
            Some(RowSchema::with_types(
                vec!["field".into()],
                vec![Some(ColumnType::Named(name.clone()))],
            )),
            None,
        ]),
        defer_routine_namespace_errors: false,
    };
    let input = RowSchema::default();
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let result = resolver
        .resolve_scalar_subquery_type_with_control(0, &input, &[], &control)
        .unwrap()
        .unwrap();
    assert_eq!(*result, ColumnType::Named(name));
    assert_eq!(budget.used(), result.reserved_bytes());
    drop(result);
    assert_eq!(budget.used(), 0);
    assert!(resolver
        .resolve_scalar_subquery_type_with_control(1, &input, &[], &control)
        .unwrap()
        .is_none());
    assert!(matches!(
        resolver.resolve_scalar_subquery_type_with_control(2, &input, &[], &control),
        Err(SQLError::Internal(_))
    ));
    let sibling = budget.reserve(budget.available() - 128).unwrap();
    let allocation = allocation_counter::measure(|| {
        assert_eq!(
            resolver
                .resolve_scalar_subquery_type_with_control(0, &input, &[], &control)
                .unwrap_err()
                .sqlstate(),
            Some("53200")
        );
    });
    assert!(allocation.bytes_total < 4096, "{allocation:?}");
    assert_eq!(budget.used(), sibling.bytes());
}
