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
    pub(crate) fn lookup_bound_sql_routine_candidates_by_binding(
        &self,
        binding: &FunctionBinding,
    ) -> Option<Vec<Arc<SQLUserFunction>>> {
        self.with_routine_lookup_registry(|registry| {
            lookup::lookup_bound_sql_routine_candidates_by_binding(registry, binding)
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
}
