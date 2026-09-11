//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind SQL routine overload resolution to shared catalog allocations and session metadata.

use crate::{Arc, Engine};
use uqa_sql::{
    ast::{ColumnType, FunctionBinding},
    routines::{
        resolution::{
            RoutineCallKind, RoutineOverloadCatalog, RoutineOverloadContext, RoutineTypeSnapshot,
        },
        RoutineResolution, SQLUserFunction, StaticFunctionMatch,
    },
    type_resolution::{BuiltinFunctionOverload, FunctionTypeResolver, ResolvedFunctionOverload},
    SQLError,
};

impl RoutineOverloadCatalog for Engine {
    fn routine_type_snapshot(&self) -> RoutineTypeSnapshot {
        self.catalog_read_view().domain_snapshot()
    }
    fn routine_search_path(&self) -> Vec<String> {
        self.session.state.read().search_path.clone()
    }
    fn has_registered_scalar_function(&self, name: &str) -> bool {
        Engine::has_registered_scalar_function(self, name)
    }
    fn lookup_sql_routine_candidates(
        &self,
        name: &str,
    ) -> Result<Option<Vec<Arc<SQLUserFunction>>>, SQLError> {
        Engine::lookup_sql_routine_candidates(self, name)
    }
    fn lookup_bound_sql_routine_candidates_by_binding(
        &self,
        binding: &FunctionBinding,
    ) -> Option<Vec<Arc<SQLUserFunction>>> {
        Engine::lookup_bound_sql_routine_candidates_by_binding(self, binding)
    }
    fn lookup_bound_sql_functions_by_binding(
        &self,
        binding: &FunctionBinding,
    ) -> Option<Vec<Arc<SQLUserFunction>>> {
        Engine::lookup_bound_sql_functions_by_binding(self, binding)
    }
}

impl RoutineResolution for Engine {
    fn lookup_bound_sql_functions_by_binding(
        &self,
        binding: &FunctionBinding,
    ) -> Option<Vec<Arc<SQLUserFunction>>> {
        Engine::lookup_bound_sql_functions_by_binding(self, binding)
    }

    fn has_registered_scalar_function(&self, name: &str) -> bool {
        Engine::has_registered_scalar_function(self, name)
    }

    fn has_registered_table_function(&self, name: &str) -> bool {
        Engine::has_registered_table_function(self, name)
    }

    fn has_registered_aggregate_function(&self, name: &str) -> bool {
        Engine::has_registered_aggregate_function(self, name)
    }

    fn lookup_visible_sql_functions(
        &self,
        name: &str,
    ) -> Result<Option<Vec<Arc<SQLUserFunction>>>, SQLError> {
        Engine::lookup_visible_sql_functions(self, name)
    }

    fn lookup_visible_sql_functions_for_analysis(
        &self,
        name: &str,
    ) -> Result<Option<Vec<Arc<SQLUserFunction>>>, SQLError> {
        Engine::lookup_visible_sql_functions_for_analysis(self, name)
    }

    fn resolve_static_sql_function(
        &self,
        name: &str,
        binding: Option<&FunctionBinding>,
        argument_names: &[Option<String>],
        argument_types: &[Option<ColumnType>],
        explicit_variadic: bool,
    ) -> Result<Option<Arc<SQLUserFunction>>, SQLError> {
        Engine::resolve_static_sql_function(
            self,
            name,
            binding,
            argument_names,
            argument_types,
            explicit_variadic,
        )
    }

    fn resolve_static_sql_function_match(
        &self,
        name: &str,
        binding: Option<&FunctionBinding>,
        argument_names: &[Option<String>],
        argument_types: &[Option<ColumnType>],
        explicit_variadic: bool,
    ) -> Result<Option<StaticFunctionMatch>, SQLError> {
        Engine::resolve_static_sql_function_match(
            self,
            name,
            binding,
            argument_names,
            argument_types,
            explicit_variadic,
        )
    }

    fn resolve_table_function_overload_with_builtins(
        &self,
        name: &str,
        binding: Option<&FunctionBinding>,
        argument_names: &[Option<String>],
        argument_types: &[Option<ColumnType>],
        explicit_variadic: bool,
        builtins: &[BuiltinFunctionOverload],
    ) -> Result<Option<ResolvedFunctionOverload>, SQLError> {
        Engine::resolve_table_function_overload_with_builtins(
            self,
            name,
            binding,
            argument_names,
            argument_types,
            explicit_variadic,
            builtins,
        )
    }
}

impl FunctionTypeResolver for Engine {
    fn has_untyped_function(&self, name: &str) -> bool {
        self.routine_overload_context().has_untyped_function(name)
    }

    fn resolve_type_name(&self, name: &str) -> Result<Option<ColumnType>, SQLError> {
        self.routine_overload_context().resolve_type_name(name)
    }

    fn resolve_function_type(
        &self,
        name: &str,
        binding: Option<&FunctionBinding>,
        argument_names: &[Option<String>],
        argument_types: &[Option<ColumnType>],
        explicit_variadic: bool,
    ) -> Result<Option<ColumnType>, SQLError> {
        self.routine_overload_context().resolve_function_type(
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
        self.routine_overload_context().resolve_function_overload(
            name,
            binding,
            argument_names,
            argument_types,
            explicit_variadic,
        )
    }

    fn is_scalar_function_binding(&self, binding: &FunctionBinding) -> Result<bool, SQLError> {
        self.routine_overload_context()
            .is_scalar_function_binding(binding)
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
        self.routine_overload_context()
            .resolve_function_overload_with_builtins(
                name,
                binding,
                argument_names,
                argument_types,
                explicit_variadic,
                builtins,
            )
    }
}

impl Engine {
    fn routine_overload_context(&self) -> RoutineOverloadContext<'_> {
        RoutineOverloadContext { catalog: self }
    }
    pub(crate) fn resolve_static_sql_function(
        &self,
        name: &str,
        binding: Option<&FunctionBinding>,
        argument_names: &[Option<String>],
        argument_types: &[Option<ColumnType>],
        explicit_variadic: bool,
    ) -> Result<Option<Arc<SQLUserFunction>>, SQLError> {
        self.routine_overload_context().resolve_static_sql_function(
            name,
            binding,
            argument_names,
            argument_types,
            explicit_variadic,
        )
    }

    pub(crate) fn resolve_static_sql_function_match(
        &self,
        name: &str,
        binding: Option<&FunctionBinding>,
        argument_names: &[Option<String>],
        argument_types: &[Option<ColumnType>],
        explicit_variadic: bool,
    ) -> Result<Option<StaticFunctionMatch>, SQLError> {
        self.routine_overload_context()
            .resolve_static_sql_function_match(
                name,
                binding,
                argument_names,
                argument_types,
                explicit_variadic,
            )
    }

    pub(crate) fn resolve_table_function_overload_with_builtins(
        &self,
        name: &str,
        binding: Option<&FunctionBinding>,
        argument_names: &[Option<String>],
        argument_types: &[Option<ColumnType>],
        explicit_variadic: bool,
        builtins: &[BuiltinFunctionOverload],
    ) -> Result<Option<ResolvedFunctionOverload>, SQLError> {
        self.routine_overload_context()
            .resolve_table_function_overload_with_builtins(
                name,
                binding,
                argument_names,
                argument_types,
                explicit_variadic,
                builtins,
            )
    }

    pub(crate) fn resolve_static_sql_routine_match(
        &self,
        name: &str,
        binding: Option<&FunctionBinding>,
        argument_names: &[Option<String>],
        argument_types: &[Option<ColumnType>],
        explicit_variadic: bool,
        kind: RoutineCallKind,
    ) -> Result<Option<StaticFunctionMatch>, SQLError> {
        self.routine_overload_context()
            .resolve_static_sql_routine_match(
                name,
                binding,
                argument_names,
                argument_types,
                explicit_variadic,
                kind,
            )
    }
}
