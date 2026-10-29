//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::routines::{
    declaration::RoutineTypeCatalog,
    resolution::{RoutineOverloadCatalog, RoutineTypeSnapshot},
    CompiledFunctionBody, RoutineBody, SQLUserFunction,
};
use crate::type_resolution::FunctionTypeResolver;
use std::sync::Arc;
use uqa_core::{memory::MemoryBudget, CancellationToken};

struct Catalog {
    function: Arc<SQLUserFunction>,
    types: RoutineTypeSnapshot,
}

impl Catalog {
    fn new(sql: &str) -> Self {
        let crate::Statement::CreateFunction(mut definition) =
            crate::compile(sql).unwrap().remove(0)
        else {
            panic!("function");
        };
        definition.object_id = Some([1; 16]);
        Self {
            types: Arc::default(),
            function: Arc::new(SQLUserFunction::new(
                *definition,
                RoutineBody::Bound(Arc::new(CompiledFunctionBody::SQL(Vec::new()))),
            )),
        }
    }
}

impl RoutineTypeCatalog for Catalog {
    fn try_describe_table(&self, _: &str) -> Result<Option<Vec<crate::ast::ColumnDef>>, String> {
        Ok(None)
    }
    fn resolve_catalog_column_type(&self, name: &str) -> Option<ColumnType> {
        ColumnType::from_sql_name(name).ok()
    }
    fn resolve_catalog_column_type_with_control(
        &self,
        name: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Produced<ColumnType>>, SQLError> {
        ColumnType::from_sql_name_with_control(name, control).map(Some)
    }
    fn resolve_catalog_column_type_name(&self, name: &str) -> Result<ColumnType, SQLError> {
        ColumnType::from_sql_name(name)
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

impl RoutineOverloadCatalog for Catalog {
    fn routine_type_snapshot(&self) -> RoutineTypeSnapshot {
        Arc::clone(&self.types)
    }
    fn routine_search_path(&self) -> Vec<String> {
        panic!("selected native namespace lookup must borrow the path")
    }
    fn routine_schema_precedes_pg_catalog(
        &self,
        schema: RelationName<'_>,
        control: &ProductionControl<'_>,
    ) -> Result<bool, SQLError> {
        search_path_precedes_pg_catalog(["public", "pg_catalog"], schema, control)
    }
    fn has_registered_scalar_function(&self, _: &str) -> bool {
        false
    }
    fn lookup_sql_routine_candidates(
        &self,
        _: &str,
    ) -> Result<Option<Vec<Arc<SQLUserFunction>>>, SQLError> {
        Ok(Some(vec![Arc::clone(&self.function)]))
    }
    fn lookup_sql_routine_candidates_with_control(
        &self,
        _: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Produced<Vec<Arc<SQLUserFunction>>>>, SQLError> {
        let mut candidates = uqa_core::memory::ProductionVec::new(*control);
        candidates.push_produced(
            control.finish(Arc::clone(&self.function), control.empty_reservation())?,
        )?;
        Ok(Some(candidates.finish()?))
    }
    fn lookup_bound_sql_routine_candidates_by_binding(
        &self,
        _: &FunctionBinding,
    ) -> Option<Vec<Arc<SQLUserFunction>>> {
        panic!("selected native routine lookup must borrow the registry")
    }
    fn lookup_bound_sql_functions_by_binding(
        &self,
        _: &FunctionBinding,
    ) -> Option<Vec<Arc<SQLUserFunction>>> {
        panic!("selected native routine lookup must borrow the registry")
    }
    fn select_bound_routine_with_control(
        &self,
        binding: &FunctionBinding,
        kind: RoutineCallKind,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Arc<SQLUserFunction>>, SQLError> {
        crate::routines::lifecycle::lookup::select_bound_routine_with_control(
            std::iter::once(&self.function),
            binding,
            kind,
            control,
        )
    }
}

fn selected(catalog: &Catalog, arguments: &[Option<ColumnType>]) -> FunctionBinding {
    RoutineOverloadContext { catalog }
        .resolve_static_sql_function_match(
            "f",
            None,
            &vec![None; arguments.len()],
            arguments,
            false,
        )
        .unwrap()
        .unwrap()
        .binding()
}

#[test]
fn selected_routine_metadata_retains_only_its_binding_and_concrete_result() {
    for (sql, types) in [
        ("CREATE FUNCTION public.f(value text) RETURNS text LANGUAGE SQL RETURN value", vec![Some(ColumnType::Text)]),
        ("CREATE FUNCTION public.f(value anyelement) RETURNS anyarray LANGUAGE SQL RETURN ARRAY[value]", vec![Some(ColumnType::Integer)]),
        ("CREATE FUNCTION public.f(VARIADIC vals integer[]) RETURNS integer LANGUAGE SQL RETURN 1", vec![Some(ColumnType::Integer), Some(ColumnType::Integer)]),
    ] {
        let catalog = Catalog::new(sql);
        let binding = selected(&catalog, &types);
        let resolver = RoutineOverloadContext { catalog: &catalog };
        let expected = resolver.resolve_function_overload("f", Some(&binding), &[], &types, false).unwrap().unwrap();
        let budget = MemoryBudget::new(1 << 20);
        let cancellation = CancellationToken::new();
        let control = ProductionControl::new(&budget, &cancellation, &cancellation);
        let mut actual = None;
        let measured = allocation_counter::measure(|| actual = Some(resolver.resolve_bound_function_overload_with_control("f", &binding, &[], &types, false, &control).unwrap()));
        let actual = actual.unwrap();
        assert_eq!(actual.binding, expected.binding);
        assert_eq!(actual.return_type, expected.return_type);
        assert!(actual.precedes_pg_catalog);
        assert_eq!(measured.bytes_current, budget.used() as i64);
        assert_eq!(budget.used(), actual.reserved_bytes());
        drop(actual);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn selected_routine_metadata_rejects_each_partial_owner_and_checks_both_cancellations() {
    let catalog =
        Catalog::new("CREATE FUNCTION public.f(value text) RETURNS text LANGUAGE SQL RETURN value");
    let types = [Some(ColumnType::Text)];
    let binding = selected(&catalog, &types);
    let resolver = RoutineOverloadContext { catalog: &catalog };
    let baseline = MemoryBudget::new(1 << 20);
    let cancellation = CancellationToken::new();
    let control = ProductionControl::new(&baseline, &cancellation, &cancellation);
    drop(
        resolver
            .resolve_bound_function_overload_with_control(
                "f",
                &binding,
                &[],
                &types,
                false,
                &control,
            )
            .unwrap(),
    );
    for available in 0..baseline.peak() {
        let budget = MemoryBudget::new(available);
        let control = ProductionControl::new(&budget, &cancellation, &cancellation);
        assert_eq!(
            resolver
                .resolve_bound_function_overload_with_control(
                    "f",
                    &binding,
                    &[],
                    &types,
                    false,
                    &control
                )
                .unwrap_err()
                .sqlstate(),
            Some("53200")
        );
        assert_eq!(budget.used(), 0);
    }
    for original in [true, false] {
        let first = CancellationToken::new();
        let second = CancellationToken::new();
        if original {
            first.cancel();
        } else {
            second.cancel();
        }
        let budget = MemoryBudget::new(1 << 20);
        let control = ProductionControl::new(&budget, &first, &second);
        assert!(control.check().is_err());
        let measured = allocation_counter::measure(|| {
            assert_eq!(
                resolver
                    .resolve_bound_function_overload_with_control(
                        "f",
                        &binding,
                        &[],
                        &types,
                        false,
                        &control
                    )
                    .unwrap_err()
                    .sqlstate(),
                Some("57014")
            );
        });
        assert_eq!(measured.count_total, 0);
        assert_eq!(budget.used(), 0);
    }
    assert_eq!(
        resolver
            .resolve_bound_function_overload_with_control("f", &binding, &[], &[], false, &control)
            .unwrap_err()
            .sqlstate(),
        Some("42883")
    );
}

#[test]
fn borrowed_namespace_precedence_preserves_first_positions_and_quoted_names() {
    for (name, path, expected) in [
        ("public.f", vec!["public", "pg_catalog", "public"], true),
        (
            "public.f",
            vec!["pg_catalog", "public", "pg_catalog"],
            false,
        ),
        ("pg_catalog.f", vec!["pg_catalog", "public"], false),
        ("\"A.Schema\".f", vec!["A.Schema", "pg_catalog"], true),
        ("public.f", vec!["public"], false),
    ] {
        let reference = RelationIdentity::borrowed_reference(name).unwrap();
        let measured = allocation_counter::measure(|| {
            assert_eq!(
                search_path_precedes_pg_catalog(
                    path.iter().copied(),
                    reference.schema.unwrap(),
                    &ProductionControl::uncontrolled()
                )
                .unwrap(),
                expected
            );
        });
        assert_eq!(measured.count_total, 0);
    }
}

#[test]
fn unbound_routine_matching_drops_workspace_and_admits_the_selected_binding() {
    for sql in [
        "CREATE FUNCTION public.f(value text) RETURNS text LANGUAGE SQL RETURN value",
        "CREATE FUNCTION public.f(value anyelement) RETURNS anyarray LANGUAGE SQL RETURN ARRAY[value]",
    ] {
        let catalog = Catalog::new(sql);
        let resolver = RoutineOverloadContext { catalog: &catalog };
        let types = [Some(ColumnType::Text)];
        let expected = resolver.resolve_function_overload("f", None, &[None], &types, false).unwrap().unwrap();
        let memory = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&memory, &token, &token);
        let mut output = None;
        let measured = allocation_counter::measure(|| {
            output = Some(resolver.resolve_function_overload_with_control("f", None, &[None], &types, false, &control).unwrap().unwrap());
        });
        let output = output.unwrap();
        assert_eq!(output.binding, expected.binding);
        assert_eq!(output.return_type, expected.return_type);
        assert_eq!(measured.bytes_current, memory.used() as i64);
        assert_eq!(output.reserved_bytes(), memory.used());
        drop(output);
        assert_eq!(memory.used(), 0);
        for available in [0, 1, memory.peak() / 2, memory.peak() - 1] {
            let budget = MemoryBudget::new(available);
            let control = ProductionControl::new(&budget, &token, &token);
            assert_eq!(resolver.resolve_function_overload_with_control("f", None, &[None], &types, false, &control).unwrap_err().sqlstate(), Some("53200"));
            assert_eq!(budget.used(), 0);
        }
    }
}

#[test]
fn controlled_combined_candidates_preserve_shadowing_and_release_rejected_owners() {
    let catalog =
        Catalog::new("CREATE FUNCTION public.f(value text) RETURNS text LANGUAGE SQL RETURN value");
    let resolver = RoutineOverloadContext { catalog: &catalog };
    let builtins = [crate::type_resolution::BuiltinFunctionOverload {
        name: "pg_catalog.f".into(),
        argument_names: vec![Some("value".into())],
        argument_types: vec![ColumnType::Text],
        return_type: ColumnType::Integer,
        default_arguments: 0,
        variadic: false,
    }];
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let mut result = None;
    let measured = allocation_counter::measure(|| {
        result = Some(
            resolver
                .resolve_function_overload_with_builtins_and_control(
                    "f",
                    None,
                    &[None],
                    &[Some(ColumnType::Text)],
                    false,
                    &builtins,
                    &control,
                )
                .unwrap()
                .unwrap(),
        );
    });
    let result = result.unwrap();
    assert!(!result.binding.builtin);
    assert_eq!(result.binding.name, "public.f");
    assert_eq!(result.return_type, ColumnType::Text);
    assert_eq!(result.reserved_bytes(), memory.used());
    assert_eq!(measured.bytes_current, memory.used() as i64);
    drop(result);
    assert_eq!(memory.used(), 0);
    for original in [true, false] {
        let first = CancellationToken::new();
        let second = CancellationToken::new();
        if original {
            first.cancel();
        } else {
            second.cancel();
        }
        let control = ProductionControl::new(&memory, &first, &second);
        assert!(control.check().is_err());
        let measured = allocation_counter::measure(|| {
            assert_eq!(
                resolver
                    .resolve_function_overload_with_builtins_and_control(
                        "f",
                        None,
                        &[None],
                        &[Some(ColumnType::Text)],
                        false,
                        &builtins,
                        &control
                    )
                    .unwrap_err()
                    .sqlstate(),
                Some("57014")
            );
        });
        assert_eq!(measured.count_total, 0);
        assert_eq!(memory.used(), 0);
    }
}
