//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Catalog-backed function and scalar-subquery typing within an owned analysis scope.

use super::snapshot::BindingSnapshot;
use crate::{
    ast::FunctionBinding, routines::RoutineResolution, BuiltinFunctionOverload, ColumnType,
    FunctionTypeResolver, ResolvedFunctionOverload, SQLError, SQLParam,
};

pub struct BindingTypeResolver<'a> {
    pub routines: &'a dyn RoutineResolution,
    pub scope: &'a BindingSnapshot,
}

impl FunctionTypeResolver for BindingTypeResolver<'_> {
    fn has_untyped_function(&self, name: &str) -> bool {
        self.routines.has_untyped_function(name)
    }

    fn resolve_type_name(&self, name: &str) -> Result<Option<ColumnType>, SQLError> {
        self.routines.resolve_type_name(name)
    }

    fn resolve_function_type(
        &self,
        name: &str,
        binding: Option<&FunctionBinding>,
        argument_names: &[Option<String>],
        argument_types: &[Option<ColumnType>],
        explicit_variadic: bool,
    ) -> Result<Option<ColumnType>, SQLError> {
        self.routines.resolve_function_type(
            name,
            binding,
            argument_names,
            argument_types,
            explicit_variadic,
        )
    }

    fn resolve_function_overload(
        &self,
        name: &str,
        binding: Option<&FunctionBinding>,
        argument_names: &[Option<String>],
        argument_types: &[Option<ColumnType>],
        explicit_variadic: bool,
    ) -> Result<Option<ResolvedFunctionOverload>, SQLError> {
        self.routines.resolve_function_overload(
            name,
            binding,
            argument_names,
            argument_types,
            explicit_variadic,
        )
    }

    fn is_scalar_function_binding(&self, binding: &FunctionBinding) -> Result<bool, SQLError> {
        self.routines.is_scalar_function_binding(binding)
    }

    fn resolve_function_overload_with_builtins(
        &self,
        name: &str,
        binding: Option<&FunctionBinding>,
        argument_names: &[Option<String>],
        argument_types: &[Option<ColumnType>],
        explicit_variadic: bool,
        builtins: &[BuiltinFunctionOverload],
    ) -> Result<Option<ResolvedFunctionOverload>, SQLError> {
        self.routines.resolve_function_overload_with_builtins(
            name,
            binding,
            argument_names,
            argument_types,
            explicit_variadic,
            builtins,
        )
    }

    fn resolve_scalar_subquery_type(
        &self,
        subquery: crate::SubqueryId,
        outer_schema: &crate::RowSchema,
        params: &[SQLParam],
    ) -> Result<Option<ColumnType>, SQLError> {
        let plan = self.scope.scalar_subqueries.get(subquery).ok_or_else(|| {
            SQLError::Internal(format!(
                "physical scalar subquery slot {subquery} is out of bounds"
            ))
        })?;
        let output = crate::binding::bind_query_plan_schema(
            self.routines,
            plan,
            params,
            &self.scope.context(),
            Some(outer_schema),
        )?;
        Ok(output.column_type(0).cloned())
    }
}
