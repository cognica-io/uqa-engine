//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind routine lookup and restoration to query snapshots, live registry guards, and catalog storage.

use crate::{open::CatalogRestoreMode, Arc, CatalogFacade, Engine, StorageBackendResult};
use uqa_execution::routines::{
    catalog,
    restoration::{self, PendingSQLFunctionRestore, RoutineRestoreContext, RoutineRestoreSchemas},
};
use uqa_sql::{
    ast::FunctionBinding,
    routines::{
        lifecycle::{lookup, names, RoutineRegistry},
        SQLUserFunction,
    },
    SQLError,
};

impl Engine {
    pub(crate) fn visible_sql_function_setness_with_control(
        &self,
        name: &str,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<uqa_sql::routines::VisibleFunctionSetness, SQLError> {
        let Some(keys) = names::routine_lookup_keys_for_analysis_with_control(self, name, control)?
        else {
            return Ok(uqa_sql::routines::VisibleFunctionSetness::default());
        };
        self.with_routine_lookup_registry(|registry| {
            lookup::visible_function_setness_by_keys_with_control(
                registry,
                keys.iter().map(String::as_str),
                control,
            )
        })
    }

    fn with_routine_lookup_registry<T>(&self, lookup: impl FnOnce(&RoutineRegistry) -> T) -> T {
        let live_registry;
        let registry = if let Some(snapshot) = self.query_sql_function_snapshots.as_ref() {
            snapshot.as_ref()
        } else {
            live_registry = self.durable.sql_user_functions.read();
            &live_registry
        };
        lookup(registry)
    }
    pub(crate) fn sql_routine_volatility_with_control(
        &self,
        name: &str,
        binding: Option<&FunctionBinding>,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<Option<uqa_sql::ast::FunctionVolatility>, SQLError> {
        if let Some(binding) = binding {
            return self.with_routine_lookup_registry(|registry| {
                lookup::bound_routine_volatility_with_control(registry, binding, control)
            });
        }
        let Some(keys) = names::routine_volatility_lookup_keys_with_control(self, name, control)?
        else {
            return Ok(None);
        };
        self.with_routine_lookup_registry(|registry| {
            lookup::routine_volatility_by_keys_with_control(
                registry,
                keys.iter().map(String::as_str),
                control,
            )
        })
    }

    pub(crate) fn lookup_visible_sql_functions(
        &self,
        name: &str,
    ) -> Result<Option<Vec<Arc<SQLUserFunction>>>, SQLError> {
        let keys = names::routine_lookup_keys(self, name)?;
        Ok(self.with_routine_lookup_registry(|registry| {
            lookup::lookup_sql_functions_by_keys(registry, keys)
        }))
    }
    pub(crate) fn lookup_visible_sql_functions_for_analysis(
        &self,
        name: &str,
    ) -> Result<Option<Vec<Arc<SQLUserFunction>>>, SQLError> {
        let Some(keys) = names::routine_lookup_keys_for_analysis(self, name)? else {
            return Ok(None);
        };
        Ok(self.with_routine_lookup_registry(|registry| {
            lookup::lookup_sql_functions_by_keys(registry, keys)
        }))
    }

    pub(crate) fn has_visible_sql_function_candidates_with_control(
        &self,
        name: &str,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<bool, SQLError> {
        let keys = uqa_sql::routines::lifecycle::names::routine_lookup_keys_with_control(
            self, name, control,
        )?;
        self.with_routine_lookup_registry(|registry| {
            for key in keys.iter() {
                control.check()?;
                if registry.get(key).is_some_and(|entries| !entries.is_empty()) {
                    return Ok(true);
                }
            }
            Ok(false)
        })
    }

    pub(crate) fn visible_sql_function_returns_set_with_control(
        &self,
        name: &str,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<bool, SQLError> {
        let Some(keys) = names::routine_lookup_keys_for_analysis_with_control(self, name, control)?
        else {
            return Ok(false);
        };
        self.with_routine_lookup_registry(|registry| {
            lookup::visible_function_returns_set_by_keys_with_control(
                registry,
                keys.iter().map(String::as_str),
                control,
            )
        })
    }
    pub(crate) fn lookup_bound_sql_functions(
        &self,
        name: &str,
    ) -> Option<Vec<Arc<SQLUserFunction>>> {
        let keys = std::iter::once(name.to_string());
        self.with_routine_lookup_registry(|registry| {
            lookup::lookup_sql_functions_by_keys(registry, keys)
        })
    }
    pub(crate) fn lookup_bound_sql_functions_by_binding(
        &self,
        binding: &FunctionBinding,
    ) -> Option<Vec<Arc<SQLUserFunction>>> {
        self.with_routine_lookup_registry(|registry| {
            lookup::lookup_bound_sql_functions_by_binding(registry, binding)
        })
    }
    pub(crate) fn lookup_sql_routine_candidates(
        &self,
        name: &str,
    ) -> Result<Option<Vec<Arc<SQLUserFunction>>>, SQLError> {
        let keys = names::routine_lookup_keys(self, name)?;
        Ok(self.with_routine_lookup_registry(|registry| {
            lookup::lookup_sql_routine_candidates_by_keys(registry, keys)
        }))
    }
    pub(crate) fn lookup_sql_routine_candidates_with_control(
        &self,
        name: &str,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<Option<uqa_core::memory::Produced<Vec<Arc<SQLUserFunction>>>>, SQLError> {
        let keys = names::routine_lookup_keys_with_control(self, name, control)?;
        self.with_routine_lookup_registry(|registry| {
            lookup::lookup_sql_routine_candidates_by_keys_with_control(
                registry,
                keys.iter().map(String::as_str),
                control,
            )
        })
    }
    pub(crate) fn lookup_bound_sql_routine_candidates_by_binding(
        &self,
        binding: &FunctionBinding,
    ) -> Option<Vec<Arc<SQLUserFunction>>> {
        self.with_routine_lookup_registry(|registry| {
            lookup::lookup_bound_sql_routine_candidates_by_binding(registry, binding)
        })
    }
    pub(crate) fn select_bound_function_with_control(
        &self,
        binding: &FunctionBinding,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<Option<Arc<SQLUserFunction>>, SQLError> {
        self.with_routine_lookup_registry(|registry| {
            lookup::select_bound_function_with_control(
                registry.values().flat_map(|overloads| overloads.iter()),
                binding,
                control,
            )
        })
    }
    pub(crate) fn select_bound_routine_with_control(
        &self,
        binding: &FunctionBinding,
        kind: uqa_sql::routines::resolution::RoutineCallKind,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<Option<Arc<SQLUserFunction>>, SQLError> {
        self.with_routine_lookup_registry(|registry| {
            lookup::select_bound_routine_with_control(
                registry.values().flat_map(|overloads| overloads.iter()),
                binding,
                kind,
                control,
            )
        })
    }
    pub(crate) fn persist_sql_functions_snapshot(
        &self,
        registry: &RoutineRegistry,
    ) -> Result<(), SQLError> {
        catalog::persist_sql_functions_snapshot(self.storage.catalog.as_deref(), registry)
    }
    fn routine_restore_context(&self) -> RoutineRestoreContext<'_> {
        RoutineRestoreContext {
            registry: self,
            roles: self,
            publication: self,
            schemas: self,
            definition: self.routine_definition_context(),
        }
    }
    pub(crate) fn install_sql_function_restore_placeholders(
        &self,
        catalog: &dyn CatalogFacade,
        mode: CatalogRestoreMode,
    ) -> StorageBackendResult<Option<PendingSQLFunctionRestore>> {
        restoration::install_sql_function_restore_placeholders(
            &self.routine_restore_context(),
            catalog,
            mode.allows_migration(),
        )
    }
    pub(crate) fn finalize_sql_function_restore(
        &self,
        pending: PendingSQLFunctionRestore,
        mode: CatalogRestoreMode,
    ) -> StorageBackendResult<()> {
        restoration::finalize_sql_function_restore(
            &self.routine_restore_context(),
            pending,
            mode.allows_migration(),
        )
    }
}
impl RoutineRestoreSchemas for Engine {
    fn routine_schema_exists(&self, schema: &str) -> bool {
        self.durable.schemas.read().contains_key(schema)
    }
    fn routine_user_type_identity(&self, type_name: &str) -> Option<String> {
        uqa_execution::catalog::projection::resolve_catalog_column_type(
            &self.catalog_execution(),
            type_name,
        )
        .and_then(|ty| ty.user_type_identity())
    }
}
