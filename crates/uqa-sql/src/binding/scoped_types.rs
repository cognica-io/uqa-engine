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
        self.routines.resolve_function_type_with_control(
            name,
            binding,
            argument_names,
            argument_types,
            explicit_variadic,
            control,
        )
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
        self.routines.resolve_function_overload_with_control(
            name,
            binding,
            argument_names,
            argument_types,
            explicit_variadic,
            control,
        )
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
        self.routines
            .resolve_function_overload_with_builtins_and_control(
                name,
                binding,
                argument_names,
                argument_types,
                explicit_variadic,
                builtins,
                control,
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

    fn composite_types(&self) -> Option<&dyn crate::expr::composites::CompositeTypeCatalog> {
        self.routines.composite_types()
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
