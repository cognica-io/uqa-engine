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
        resolution::{RoutineOverloadCatalog, RoutineOverloadContext, RoutineTypeSnapshot},
        RoutineResolution, SQLUserFunction, StaticFunctionMatch,
    },
    type_resolution::{BuiltinFunctionOverload, FunctionTypeResolver, ResolvedFunctionOverload},
    SQLError,
};

impl RoutineOverloadCatalog for Engine {
    fn routine_type_snapshot(&self) -> RoutineTypeSnapshot {
        self.query_catalog_snapshot.as_ref().map_or_else(
            || self.durable.domains.snapshot(),
            |snapshot| Arc::clone(&snapshot.domains),
        )
    }
    fn routine_search_path(&self) -> Vec<String> {
        crate::session::effective_search_path(&self.session.state.read())
    }
    fn routine_schema_precedes_pg_catalog(
        &self,
        schema: uqa_core::RelationName<'_>,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<bool, SQLError> {
        control.check()?;
        let state = self.session.state.read();
        let current_user = state.authorization.current();
        uqa_sql::routines::resolution::search_path_precedes_pg_catalog(
            state.search_path.iter().map(|entry| {
                if entry == "$user" {
                    current_user.name.as_str()
                } else {
                    entry.as_str()
                }
            }),
            schema,
            control,
        )
    }
    fn has_registered_scalar_function_with_control(
        &self,
        name: &str,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<bool, SQLError> {
        self.query_runtime_view()
            .has_scalar_function_with_control(name, control)
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
    fn lookup_sql_routine_candidates_with_control(
        &self,
        name: &str,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<Option<uqa_core::memory::Produced<Vec<Arc<SQLUserFunction>>>>, SQLError> {
        Engine::lookup_sql_routine_candidates_with_control(self, name, control)
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
    fn select_bound_routine_with_control(
        &self,
        binding: &FunctionBinding,
        kind: uqa_sql::routines::resolution::RoutineCallKind,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<Option<Arc<SQLUserFunction>>, SQLError> {
        Engine::select_bound_routine_with_control(self, binding, kind, control)
    }
}

impl RoutineResolution for Engine {
    fn routine_body(
        &self,
        function: &SQLUserFunction,
    ) -> Result<Arc<uqa_sql::routines::CompiledFunctionBody>, SQLError> {
        self.session.routine_bodies.body(function, |def| {
            uqa_execution::routines::invocation::bodies::compile_session_body(
                self,
                &self.routine_definition_context().compilation.analysis,
                def,
            )
        })
    }

    fn routine_analysis_body(
        &self,
        function: &SQLUserFunction,
    ) -> Result<Arc<uqa_sql::routines::CompiledFunctionBody>, SQLError> {
        if matches!(function.body, uqa_sql::routines::RoutineBody::Source) {
            self.session.routine_bodies.inspect(function, |definition| {
                uqa_execution::routines::invocation::bodies::compile_analysis_body(
                    self,
                    &self.routine_compilation_context(),
                    definition,
                )
            })
        } else {
            self.routine_body(function)
        }
    }

    fn lookup_bound_sql_functions(&self, name: &str) -> Option<Vec<Arc<SQLUserFunction>>> {
        Engine::lookup_bound_sql_functions(self, name)
    }

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

    fn has_registered_scalar_function_with_control(
        &self,
        name: &str,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<bool, SQLError> {
        self.query_runtime_view()
            .has_scalar_function_with_control(name, control)
    }

    fn has_registered_table_function_with_control(
        &self,
        name: &str,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<bool, SQLError> {
        self.query_runtime_view()
            .has_table_function_with_control(name, control)
    }

    fn has_registered_aggregate_function_with_control(
        &self,
        name: &str,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<bool, SQLError> {
        self.query_runtime_view()
            .has_aggregate_function_with_control(name, control)
    }

    fn registered_table_function_columns(&self, name: &str) -> Option<Vec<String>> {
        self.query_runtime_view()
            .lookup_table_function(&name.to_ascii_lowercase())?
            .function
            .output_columns()
            .map(<[String]>::to_vec)
    }

    fn registered_table_function_columns_with_control(
        &self,
        name: &str,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<Option<uqa_core::memory::Produced<Vec<String>>>, SQLError> {
        let name = uqa_sql::semantics::lowercase_function_name_with_control(name, control)?;
        let Some(registered) = self.query_runtime_view().lookup_table_function(&name) else {
            return Ok(None);
        };
        registered
            .function
            .output_columns()
            .map(|columns| {
                let mut output = uqa_core::memory::ProductionVec::new(*control);
                for column in columns {
                    output.push_produced(control.copy_text(column)?)?;
                }
                Ok(output.finish()?)
            })
            .transpose()
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

    fn has_visible_sql_functions_with_control(
        &self,
        name: &str,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<bool, SQLError> {
        self.has_visible_sql_function_candidates_with_control(name, control)
    }

    fn lookup_visible_sql_functions_for_analysis(
        &self,
        name: &str,
    ) -> Result<Option<Vec<Arc<SQLUserFunction>>>, SQLError> {
        Engine::lookup_visible_sql_functions_for_analysis(self, name)
    }

    fn visible_function_returns_set_with_control(
        &self,
        name: &str,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<bool, SQLError> {
        self.visible_sql_function_returns_set_with_control(name, control)
    }

    fn visible_function_setness_with_control(
        &self,
        name: &str,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<uqa_sql::routines::VisibleFunctionSetness, SQLError> {
        self.visible_sql_function_setness_with_control(name, control)
    }

    fn select_bound_function_with_control(
        &self,
        binding: &FunctionBinding,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<Option<Arc<SQLUserFunction>>, SQLError> {
        Engine::select_bound_function_with_control(self, binding, control)
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

    fn resolve_static_sql_function_setness_with_control(
        &self,
        name: &str,
        binding: Option<&FunctionBinding>,
        argument_names: &[Option<String>],
        argument_types: &[Option<ColumnType>],
        explicit_variadic: bool,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<Option<bool>, SQLError> {
        let Some(binding) = self
            .routine_overload_context()
            .resolve_static_sql_function_binding_with_control(
                name,
                binding,
                argument_names,
                argument_types,
                explicit_variadic,
                control,
            )?
        else {
            return Ok(None);
        };
        Ok(self
            .select_bound_function_with_control(&binding, control)?
            .map(|function| function.def.returns_set()))
    }

    fn resolve_static_sql_function_match_with_control(
        &self,
        name: &str,
        binding: Option<&FunctionBinding>,
        call: uqa_sql::type_resolution::RoutineCallDescriptor<'_>,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<Option<uqa_sql::routines::resolution::OwnedStaticFunctionMatch>, SQLError> {
        self.routine_overload_context()
            .resolve_static_sql_routine_match_with_control(
                name,
                binding,
                call.argument_names,
                call.argument_types,
                call.explicit_variadic,
                uqa_sql::routines::resolution::RoutineCallKind::Function,
                control,
            )
    }

    fn resolve_static_sql_function_binding_with_control(
        &self,
        name: &str,
        binding: Option<&FunctionBinding>,
        argument_names: &[Option<String>],
        argument_types: &[Option<ColumnType>],
        explicit_variadic: bool,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<Option<uqa_core::memory::Produced<FunctionBinding>>, SQLError> {
        self.routine_overload_context()
            .resolve_static_sql_function_binding_with_control(
                name,
                binding,
                argument_names,
                argument_types,
                explicit_variadic,
                control,
            )
    }

    fn resolve_table_function_overload_with_builtins_and_control(
        &self,
        name: &str,
        binding: Option<&FunctionBinding>,
        call: uqa_sql::type_resolution::RoutineCallDescriptor<'_>,
        builtins: &[BuiltinFunctionOverload],
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<Option<uqa_core::memory::Produced<ResolvedFunctionOverload>>, SQLError> {
        self.routine_overload_context()
            .resolve_table_function_overload_with_builtins_and_control(
                name, binding, call, builtins, control,
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
    fn format_type_name(&self, ty: &ColumnType) -> Result<String, SQLError> {
        self.routine_overload_context().format_type_name(ty)
    }

    fn format_type_name_with_control(
        &self,
        ty: &ColumnType,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<uqa_core::memory::Produced<String>, SQLError> {
        self.routine_overload_context()
            .format_type_name_with_control(ty, control)
    }

    fn has_untyped_function_with_control(
        &self,
        name: &str,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<bool, SQLError> {
        self.query_runtime_view()
            .has_scalar_function_with_control(name, control)
    }

    fn has_untyped_function(&self, name: &str) -> bool {
        self.routine_overload_context().has_untyped_function(name)
    }

    fn resolve_type_name(&self, name: &str) -> Result<Option<ColumnType>, SQLError> {
        self.routine_overload_context().resolve_type_name(name)
    }

    fn resolve_type_name_with_control(
        &self,
        name: &str,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<Option<uqa_core::memory::Produced<ColumnType>>, SQLError> {
        self.routine_overload_context()
            .resolve_type_name_with_control(name, control)
    }

    fn resolve_collation_name(&self, name: &str) -> Result<Option<u32>, SQLError> {
        uqa_execution::catalog::projection::resolve_expression_collation_oid(
            &self.catalog_execution(),
            name,
        )
        .map(Some)
    }

    fn resolve_collation_name_with_control(
        &self,
        name: &str,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<Option<u32>, SQLError> {
        uqa_execution::catalog::projection::resolve_expression_collation_oid_with_control(
            &self.catalog_execution(),
            name,
            control,
        )
        .map(Some)
    }

    fn enum_labels(&self) -> Option<&dyn uqa_sql::expr::enums::EnumLabelCatalog> {
        Some(self)
    }

    fn catalog_input_functions(&self) -> Option<&dyn uqa_sql::expr::CatalogInputFunctions> {
        Some(self)
    }

    fn composite_types(&self) -> Option<&dyn uqa_sql::expr::composites::CompositeTypeCatalog> {
        Some(self)
    }

    fn require_type_usage(&self, ty: &ColumnType) -> Result<(), SQLError> {
        uqa_execution::catalog::security::type_inquiry::require_type_usage(
            &self.catalog_execution(),
            ty,
        )
    }

    fn resolve_function_type_with_control(
        &self,
        name: &str,
        binding: Option<&FunctionBinding>,
        argument_names: &[Option<String>],
        argument_types: &[Option<ColumnType>],
        explicit_variadic: bool,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<Option<uqa_core::memory::Produced<ColumnType>>, SQLError> {
        self.routine_overload_context()
            .resolve_function_type_with_control(
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
        binding: Option<&FunctionBinding>,
        argument_names: &[Option<String>],
        argument_types: &[Option<ColumnType>],
        explicit_variadic: bool,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<Option<uqa_core::memory::Produced<ResolvedFunctionOverload>>, SQLError> {
        self.routine_overload_context()
            .resolve_function_overload_with_control(
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
        binding: Option<&FunctionBinding>,
        argument_names: &[Option<String>],
        argument_types: &[Option<ColumnType>],
        explicit_variadic: bool,
        builtins: &[BuiltinFunctionOverload],
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<Option<uqa_core::memory::Produced<ResolvedFunctionOverload>>, SQLError> {
        self.routine_overload_context()
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

    fn is_scalar_function_binding_with_control(
        &self,
        binding: &FunctionBinding,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<bool, SQLError> {
        self.routine_overload_context()
            .is_scalar_function_binding_with_control(binding, control)
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

impl uqa_sql::expr::CatalogInputFunctions for Engine {
    fn read_unknown_input_with_control(
        &self,
        text: &str,
        target: &ColumnType,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<uqa_core::memory::Produced<uqa_core::Value>, SQLError> {
        uqa_sql::expr::read_catalog_input_with_control(text, target, self, control)
    }

    fn read_unknown_input(
        &self,
        text: &str,
        target: &ColumnType,
    ) -> Result<uqa_core::Value, SQLError> {
        uqa_sql::expr::read_catalog_input(text, target, self)
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
}
