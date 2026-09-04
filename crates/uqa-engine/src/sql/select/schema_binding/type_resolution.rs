//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Function and scalar-subquery type resolution for schema binding.

use uqa_execution::{FunctionTypeResolver, ResolvedFunctionOverload, RowSchema};
use uqa_sql::ast::{ColumnType, SetOpKind};
use uqa_sql::{SQLError, SQLParam};

use crate::engine_user_functions::RoutineResolution;

use super::{expr_contains_subquery, scope::merge_types, QueryPlan, ScalarExpr, SchemaScope};

pub(super) fn set_operation_output_schema(
    left: &RowSchema,
    right: &RowSchema,
    kind: SetOpKind,
    all: bool,
) -> Result<RowSchema, SQLError> {
    let types = left
        .column_types()
        .iter()
        .zip(right.column_types())
        .map(|(left, right)| merge_types(left.as_ref(), right.as_ref()))
        .collect::<Result<Vec<_>, _>>()?;
    if !matches!((kind, all), (SetOpKind::Union, true)) {
        for ty in types.iter().flatten() {
            uqa_execution::require_equality_operator(ty)?;
        }
    }
    Ok(RowSchema::with_types(left.columns().to_vec(), types))
}

pub(super) struct QueryFunctionTypeResolver<'a> {
    pub(super) routines: &'a dyn RoutineResolution,
    pub(super) scalar_subquery_types: Option<Vec<Option<ColumnType>>>,
    pub(super) defer_routine_namespace_errors: bool,
}

impl SchemaScope {
    pub(super) fn query_function_type_resolver<'a>(
        &mut self,
        routines: &'a dyn RoutineResolution,
        expression: &ScalarExpr,
        schema: &RowSchema,
        subqueries: &[QueryPlan],
        params: &[SQLParam],
        outer: Option<&RowSchema>,
    ) -> Result<QueryFunctionTypeResolver<'a>, SQLError> {
        self.query_function_type_resolver_for_subqueries(
            routines,
            expr_contains_subquery(expression),
            schema,
            subqueries,
            params,
            outer,
        )
    }

    pub(super) fn query_function_type_resolver_for_subqueries<'a>(
        &mut self,
        routines: &'a dyn RoutineResolution,
        contains_subquery: bool,
        schema: &RowSchema,
        subqueries: &[QueryPlan],
        params: &[SQLParam],
        outer: Option<&RowSchema>,
    ) -> Result<QueryFunctionTypeResolver<'a>, SQLError> {
        if subqueries.is_empty() || !contains_subquery {
            return Ok(QueryFunctionTypeResolver {
                routines,
                scalar_subquery_types: None,
                defer_routine_namespace_errors: false,
            });
        }
        let subquery_outer = self.validate_references.then_some(schema).or(outer);
        let scalar_subquery_types = subqueries
            .iter()
            .map(|plan| {
                self.bind_query(routines, plan, params, subquery_outer)
                    .map(|output| output.column_type(0).cloned())
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(QueryFunctionTypeResolver {
            routines,
            scalar_subquery_types: Some(scalar_subquery_types),
            defer_routine_namespace_errors: false,
        })
    }
}

impl QueryFunctionTypeResolver<'_> {
    fn routine_resolution<T>(
        &self,
        result: Result<Option<T>, SQLError>,
    ) -> Result<Option<T>, SQLError> {
        match result {
            Err(error)
                if self.defer_routine_namespace_errors
                    && crate::engine_user_functions::is_routine_namespace_lookup_error(&error) =>
            {
                Ok(None)
            }
            result => result,
        }
    }
}

impl FunctionTypeResolver for QueryFunctionTypeResolver<'_> {
    fn has_untyped_function(&self, name: &str) -> bool {
        self.routines.has_untyped_function(name)
    }

    fn resolve_type_name(&self, name: &str) -> Result<Option<ColumnType>, SQLError> {
        self.routines.resolve_type_name(name)
    }

    fn resolve_function_type(
        &self,
        name: &str,
        binding: Option<&uqa_sql::ast::FunctionBinding>,
        argument_names: &[Option<String>],
        argument_types: &[Option<ColumnType>],
        explicit_variadic: bool,
    ) -> Result<Option<ColumnType>, SQLError> {
        self.routine_resolution(self.routines.resolve_function_type(
            name,
            binding,
            argument_names,
            argument_types,
            explicit_variadic,
        ))
    }

    fn resolve_function_overload(
        &self,
        name: &str,
        binding: Option<&uqa_sql::ast::FunctionBinding>,
        argument_names: &[Option<String>],
        argument_types: &[Option<ColumnType>],
        explicit_variadic: bool,
    ) -> Result<Option<ResolvedFunctionOverload>, SQLError> {
        self.routine_resolution(self.routines.resolve_function_overload(
            name,
            binding,
            argument_names,
            argument_types,
            explicit_variadic,
        ))
    }

    fn is_scalar_function_binding(
        &self,
        binding: &uqa_sql::ast::FunctionBinding,
    ) -> Result<bool, SQLError> {
        self.routines.is_scalar_function_binding(binding)
    }

    fn resolve_function_overload_with_builtins(
        &self,
        name: &str,
        binding: Option<&uqa_sql::ast::FunctionBinding>,
        argument_names: &[Option<String>],
        argument_types: &[Option<ColumnType>],
        explicit_variadic: bool,
        builtins: &[uqa_execution::BuiltinFunctionOverload],
    ) -> Result<Option<ResolvedFunctionOverload>, SQLError> {
        self.routine_resolution(self.routines.resolve_function_overload_with_builtins(
            name,
            binding,
            argument_names,
            argument_types,
            explicit_variadic,
            builtins,
        ))
    }

    fn resolve_scalar_subquery_type(
        &self,
        subquery: uqa_execution::SubqueryId,
        outer_schema: &RowSchema,
        params: &[SQLParam],
    ) -> Result<Option<ColumnType>, SQLError> {
        let Some(types) = self.scalar_subquery_types.as_ref() else {
            return self
                .routines
                .resolve_scalar_subquery_type(subquery, outer_schema, params);
        };
        types.get(subquery).cloned().ok_or_else(|| {
            SQLError::Internal(format!("scalar subquery slot {subquery} is out of bounds"))
        })
    }
}
