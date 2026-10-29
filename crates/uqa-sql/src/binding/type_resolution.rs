//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Function and scalar-subquery type resolution for schema binding.

use crate::ast::ColumnType;
use crate::{FunctionTypeResolver, ResolvedFunctionOverload, RowSchema};
use crate::{SQLError, SQLParam};

use crate::routines::RoutineResolution;

use super::{QueryPlan, ScalarExpr, SchemaScope};

#[cfg(test)]
mod production_tests;
mod query_results;
pub(super) use query_results::ValidatedSubqueries;

pub(super) struct QueryFunctionTypeResolver<'a> {
    pub(super) routines: &'a dyn RoutineResolution,
    pub(super) scalar_subquery_schemas: Option<Vec<Option<RowSchema>>>,
    pub(super) defer_routine_namespace_errors: bool,
}

/// Results of the immediately preceding reference check, borrowed only while the same condition is checked with fixed parameters.
pub(super) struct BoundConditionQueries<'a> {
    pub(super) plans: &'a [QueryPlan],
    pub(super) input: &'a RowSchema,
    pub(super) schemas: Option<&'a [Option<RowSchema>]>,
}

impl BoundConditionQueries<'_> {
    pub(super) fn get(
        &self,
        index: usize,
        input: &RowSchema,
        plans: &[QueryPlan],
    ) -> Option<&RowSchema> {
        if !std::ptr::eq(self.plans, plans) || self.input != input {
            return None;
        }
        self.schemas?.get(index)?.as_ref()
    }
}

impl SchemaScope {
    pub(super) fn query_function_type_resolver<'a>(
        &mut self,
        routines: &'a dyn RoutineResolution,
        expression: &ScalarExpr,
        schema: &RowSchema,
        subqueries: &[QueryPlan],
        params: &[SQLParam],
    ) -> Result<QueryFunctionTypeResolver<'a>, SQLError> {
        self.query_function_type_resolver_for_subqueries(
            routines,
            std::slice::from_ref(expression),
            schema,
            subqueries,
            params,
        )
    }

    pub(super) fn query_function_type_resolver_for_subqueries<'a>(
        &mut self,
        routines: &'a dyn RoutineResolution,
        expressions: &[ScalarExpr],
        schema: &RowSchema,
        subqueries: &[QueryPlan],
        params: &[SQLParam],
    ) -> Result<QueryFunctionTypeResolver<'a>, SQLError> {
        ValidatedSubqueries::new(routines, subqueries, params).resolver(self, expressions, schema)
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
                    && crate::routines::is_routine_namespace_lookup_error(&error) =>
            {
                Ok(None)
            }
            result => result,
        }
    }
}

impl FunctionTypeResolver for QueryFunctionTypeResolver<'_> {
    fn resolve_collation_name_with_control(
        &self,
        name: &str,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<Option<u32>, SQLError> {
        self.routines
            .resolve_collation_name_with_control(name, control)
    }

    fn is_scalar_function_binding_with_control(
        &self,
        binding: &crate::ast::FunctionBinding,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<bool, SQLError> {
        self.routines
            .is_scalar_function_binding_with_control(binding, control)
    }

    fn resolve_scalar_subquery_type_with_control(
        &self,
        subquery: crate::SubqueryId,
        outer_schema: &RowSchema,
        params: &[SQLParam],
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<Option<uqa_core::memory::Produced<ColumnType>>, SQLError> {
        control.check()?;
        let Some(schemas) = self.scalar_subquery_schemas.as_ref() else {
            return self.routines.resolve_scalar_subquery_type_with_control(
                subquery,
                outer_schema,
                params,
                control,
            );
        };
        schemas
            .get(subquery)
            .ok_or_else(|| {
                SQLError::Internal(format!("scalar subquery slot {subquery} is out of bounds"))
            })?
            .as_ref()
            .and_then(|schema| schema.column_type(0))
            .map(|ty| ty.clone_with_control(control))
            .transpose()
            .map_err(Into::into)
    }
    fn format_type_name_with_control(
        &self,
        ty: &crate::ColumnType,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<uqa_core::memory::Produced<String>, SQLError> {
        self.routines.format_type_name_with_control(ty, control)
    }

    fn resolve_type_name_with_control(
        &self,
        name: &str,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<Option<uqa_core::memory::Produced<crate::ColumnType>>, SQLError> {
        self.routines.resolve_type_name_with_control(name, control)
    }

    fn resolve_function_type_with_control(
        &self,
        name: &str,
        binding: Option<&crate::ast::FunctionBinding>,
        argument_names: &[Option<String>],
        argument_types: &[Option<crate::ColumnType>],
        explicit_variadic: bool,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<Option<uqa_core::memory::Produced<crate::ColumnType>>, SQLError> {
        self.routine_resolution(self.routines.resolve_function_type_with_control(
            name,
            binding,
            argument_names,
            argument_types,
            explicit_variadic,
            control,
        ))
    }

    fn resolve_function_overload_with_control(
        &self,
        name: &str,
        binding: Option<&crate::ast::FunctionBinding>,
        argument_names: &[Option<String>],
        argument_types: &[Option<crate::ColumnType>],
        explicit_variadic: bool,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<Option<uqa_core::memory::Produced<ResolvedFunctionOverload>>, SQLError> {
        self.routine_resolution(self.routines.resolve_function_overload_with_control(
            name,
            binding,
            argument_names,
            argument_types,
            explicit_variadic,
            control,
        ))
    }

    fn resolve_function_overload_with_builtins_and_control(
        &self,
        name: &str,
        binding: Option<&crate::ast::FunctionBinding>,
        argument_names: &[Option<String>],
        argument_types: &[Option<crate::ColumnType>],
        explicit_variadic: bool,
        builtins: &[crate::BuiltinFunctionOverload],
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<Option<uqa_core::memory::Produced<ResolvedFunctionOverload>>, SQLError> {
        self.routine_resolution(
            self.routines
                .resolve_function_overload_with_builtins_and_control(
                    name,
                    binding,
                    argument_names,
                    argument_types,
                    explicit_variadic,
                    builtins,
                    control,
                ),
        )
    }

    fn format_type_name(&self, ty: &ColumnType) -> Result<String, SQLError> {
        self.routines.format_type_name(ty)
    }

    fn has_untyped_function(&self, name: &str) -> bool {
        self.routines.has_untyped_function(name)
    }

    fn has_untyped_function_with_control(
        &self,
        name: &str,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<bool, SQLError> {
        self.routines
            .has_untyped_function_with_control(name, control)
    }

    fn resolve_type_name(&self, name: &str) -> Result<Option<ColumnType>, SQLError> {
        self.routines.resolve_type_name(name)
    }

    fn resolve_collation_name(&self, name: &str) -> Result<Option<u32>, SQLError> {
        self.routines.resolve_collation_name(name)
    }

    fn enum_labels(&self) -> Option<&dyn crate::expr::enums::EnumLabelCatalog> {
        self.routines.enum_labels()
    }

    fn catalog_input_functions(&self) -> Option<&dyn crate::expr::CatalogInputFunctions> {
        self.routines.catalog_input_functions()
    }

    fn composite_types(&self) -> Option<&dyn crate::expr::composites::CompositeTypeCatalog> {
        self.routines.composite_types()
    }

    fn resolve_function_type(
        &self,
        name: &str,
        binding: Option<&crate::ast::FunctionBinding>,
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
        binding: Option<&crate::ast::FunctionBinding>,
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
        binding: &crate::ast::FunctionBinding,
    ) -> Result<bool, SQLError> {
        self.routines.is_scalar_function_binding(binding)
    }

    fn resolve_function_overload_with_builtins(
        &self,
        name: &str,
        binding: Option<&crate::ast::FunctionBinding>,
        argument_names: &[Option<String>],
        argument_types: &[Option<ColumnType>],
        explicit_variadic: bool,
        builtins: &[crate::BuiltinFunctionOverload],
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
        subquery: crate::SubqueryId,
        outer_schema: &RowSchema,
        params: &[SQLParam],
    ) -> Result<Option<ColumnType>, SQLError> {
        let Some(schemas) = self.scalar_subquery_schemas.as_ref() else {
            return self
                .routines
                .resolve_scalar_subquery_type(subquery, outer_schema, params);
        };
        schemas
            .get(subquery)
            .map(|schema| {
                schema
                    .as_ref()
                    .and_then(|schema| schema.column_type(0))
                    .cloned()
            })
            .ok_or_else(|| {
                SQLError::Internal(format!("scalar subquery slot {subquery} is out of bounds"))
            })
    }
}
