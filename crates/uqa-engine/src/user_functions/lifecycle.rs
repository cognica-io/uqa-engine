//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Routine registration, catalog persistence, alteration, and removal.

mod rename;

use std::collections::{BTreeMap, BTreeSet};

use uqa_sql::ast::{CreateFunction, FunctionBinding, FunctionBody};
use uqa_sql::SQLError;

use crate::{
    open::CatalogRestoreMode, Arc, CatalogFacade, Engine, RelationIdentity, StorageBackendError,
    StorageBackendResult, FUNCTIONS_METADATA_KEY,
};

use super::resolution::routine_signature_types;
use super::{CompiledFunctionBody, SQLUserFunction};
use uqa_sql::routines::dependencies::RoutineCompilationMode;
pub(super) use uqa_sql::routines::lifecycle::routine_signature_label;

pub(crate) struct PendingSQLFunctionRestore {
    definitions: BTreeMap<String, Vec<CreateFunction>>,
    migrated: bool,
    previous: BTreeMap<String, Vec<Arc<SQLUserFunction>>>,
}

impl Engine {
    /// Resolve a routine name through the schemas the current user can access, while qualified names report missing schemas and `USAGE` denials directly.
    fn routine_lookup_keys(&self, name: &str) -> Result<Vec<String>, SQLError> {
        uqa_sql::routines::lifecycle::names::routine_lookup_keys(self, name)
    }

    /// Visible overload set for `name`. Identical signatures in later
    /// `search_path` schemas are shadowed while distinct signatures remain
    /// candidates, matching `PostgreSQL`'s routine lookup rules.
    pub(crate) fn lookup_visible_sql_functions(
        &self,
        name: &str,
    ) -> Result<Option<Vec<Arc<SQLUserFunction>>>, SQLError> {
        let keys = self.routine_lookup_keys(name)?;
        Ok(self.lookup_sql_functions_by_keys(keys))
    }

    /// Inspect accessible overloads without reporting namespace errors before recursive argument and reference validation. The definitive binder performs the checked lookup again before it records a routine identity.
    pub(crate) fn lookup_visible_sql_functions_for_analysis(
        &self,
        name: &str,
    ) -> Result<Option<Vec<Arc<SQLUserFunction>>>, SQLError> {
        match self.lookup_visible_sql_functions(name) {
            Err(error) if super::is_routine_namespace_lookup_error(&error) => Ok(None),
            result => result,
        }
    }

    /// Resolve an already-bound routine identity without repeating namespace-name access checks. `PostgreSQL` stores object identities in views, generated expressions, and SQL-standard routine bodies, so later users need object privileges but do not re-resolve the original schema-qualified name.
    pub(crate) fn lookup_bound_sql_functions(
        &self,
        name: &str,
    ) -> Option<Vec<Arc<SQLUserFunction>>> {
        self.lookup_sql_functions_by_keys(std::iter::once(name.to_string()))
    }

    /// Resolve a catalog-bound routine by its durable object identity. Legacy bindings without an identity retain exact canonical-name lookup only during catalog migration.
    pub(crate) fn lookup_bound_sql_functions_by_binding(
        &self,
        binding: &FunctionBinding,
    ) -> Option<Vec<Arc<SQLUserFunction>>> {
        let Some(object_id) = binding.object_id else {
            return self.lookup_bound_sql_functions(&binding.name);
        };
        let live_registry;
        let registry = if let Some(snapshot) = self.query_sql_function_snapshots.as_ref() {
            snapshot.as_ref()
        } else {
            live_registry = self.durable.sql_user_functions.read();
            &live_registry
        };
        let matches = registry
            .values()
            .flat_map(|overloads| overloads.iter())
            .filter(|function| function.def.object_id == Some(object_id))
            .cloned()
            .collect::<Vec<_>>();
        (!matches.is_empty()).then_some(matches)
    }

    fn lookup_sql_functions_by_keys(
        &self,
        keys: impl IntoIterator<Item = String>,
    ) -> Option<Vec<Arc<SQLUserFunction>>> {
        let live_registry;
        let registry = if let Some(snapshot) = self.query_sql_function_snapshots.as_ref() {
            snapshot.as_ref()
        } else {
            live_registry = self.durable.sql_user_functions.read();
            &live_registry
        };
        let mut visible = Vec::new();
        let mut seen = std::collections::BTreeSet::new();
        for key in keys {
            let Some(overloads) = registry.get(&key) else {
                continue;
            };
            for function in overloads {
                let identity = (
                    routine_signature_types(&function.def),
                    function.def.is_procedure,
                );
                if seen.insert(identity) {
                    visible.push(function.clone());
                }
            }
        }
        (!visible.is_empty()).then_some(visible)
    }

    /// Call-resolution candidates before search-path shadowing. Named notation can make a later identical declared signature visible when an earlier routine uses different parameter names, so structural matching must happen first.
    pub(super) fn lookup_sql_routine_candidates(
        &self,
        name: &str,
    ) -> Result<Option<Vec<Arc<SQLUserFunction>>>, SQLError> {
        let keys = self.routine_lookup_keys(name)?;
        Ok(self.lookup_sql_routine_candidates_by_keys(keys))
    }

    pub(super) fn lookup_bound_sql_routine_candidates_by_binding(
        &self,
        binding: &FunctionBinding,
    ) -> Option<Vec<Arc<SQLUserFunction>>> {
        if binding.object_id.is_some() {
            self.lookup_bound_sql_functions_by_binding(binding)
        } else {
            self.lookup_sql_routine_candidates_by_keys(std::iter::once(binding.name.clone()))
        }
    }

    fn lookup_sql_routine_candidates_by_keys(
        &self,
        keys: impl IntoIterator<Item = String>,
    ) -> Option<Vec<Arc<SQLUserFunction>>> {
        let live_registry;
        let registry = if let Some(snapshot) = self.query_sql_function_snapshots.as_ref() {
            snapshot.as_ref()
        } else {
            live_registry = self.durable.sql_user_functions.read();
            &live_registry
        };
        let candidates = keys
            .into_iter()
            .filter_map(|key| registry.get(&key))
            .flat_map(|overloads| overloads.iter().cloned())
            .collect::<Vec<_>>();
        (!candidates.is_empty()).then_some(candidates)
    }

    /// Current nesting cap for user-defined routine calls.
    pub fn sql_function_depth_limit(&self) -> usize {
        self.runtime
            .function_depth_limit
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Adjust the nesting cap for user-defined routine calls
    /// (minimum 1). Mirrors `PostgreSQL`'s `max_stack_depth` role for
    /// recursive functions.
    pub fn set_sql_function_depth_limit(&self, limit: usize) {
        self.runtime
            .function_depth_limit
            .store(limit.max(1), std::sync::atomic::Ordering::Relaxed);
    }

    /// Queue a notice (`RAISE NOTICE` / `WARNING` / ...).
    pub(crate) fn push_sql_notice(&self, level: &str, message: &str) {
        self.query_runtime_view().push_diagnostic(level, message);
    }

    /// Drain queued notices as `(level, message)` pairs in emission
    /// order.
    pub fn take_sql_notices(&self) -> Vec<(String, String)> {
        std::mem::take(&mut *self.runtime.notices.lock())
    }

    pub(crate) fn persist_sql_functions_snapshot(
        &self,
        registry: &BTreeMap<String, Vec<Arc<SQLUserFunction>>>,
    ) -> Result<(), SQLError> {
        let Some(catalog) = self.storage.catalog.as_ref() else {
            return Ok(());
        };
        let defs: BTreeMap<String, Vec<CreateFunction>> = registry
            .iter()
            .map(|(name, overloads)| {
                (
                    name.clone(),
                    overloads
                        .iter()
                        .map(|function| function.def.clone())
                        .collect(),
                )
            })
            .collect();
        let json = serde_json::to_string(&defs)
            .map_err(|err| SQLError::Internal(format!("serialize function catalog: {err}")))?;
        catalog
            .set_metadata(FUNCTIONS_METADATA_KEY, &json)
            .map_err(|err| SQLError::Internal(format!("persist function catalog: {err}")))
    }

    fn canonicalize_persisted_sql_functions(
        &self,
        defs: BTreeMap<String, Vec<CreateFunction>>,
    ) -> StorageBackendResult<(BTreeMap<String, Vec<CreateFunction>>, bool)> {
        let mut canonical_defs: BTreeMap<String, Vec<CreateFunction>> = BTreeMap::new();
        let mut object_ids = BTreeSet::new();
        let mut migrated = false;
        for (stored_name, overloads) in defs {
            let stored_relation =
                RelationIdentity::from_legacy_name(&stored_name).map_err(|error| {
                    StorageBackendError::Other(format!(
                        "invalid persisted routine registry key `{stored_name}`: {error}"
                    ))
                })?;
            if !self
                .durable
                .schemas
                .read()
                .contains_key(&stored_relation.schema)
            {
                return Err(StorageBackendError::Other(format!(
                    "persisted routine `{stored_name}` references missing schema `{}`",
                    stored_relation.schema
                )));
            }
            let canonical_name = stored_relation.qualified_name();
            for mut def in overloads {
                if def.object_id.is_none() || def.object_id == Some([0; 16]) {
                    def.object_id = Some(crate::new_routine_object_id()?);
                    migrated = true;
                }
                let object_id = def.object_id.ok_or_else(|| {
                    StorageBackendError::Other(format!(
                        "persisted routine `{stored_name}` has no object identity"
                    ))
                })?;
                if !object_ids.insert(object_id) {
                    return Err(StorageBackendError::Other(format!(
                        "duplicate persisted routine object identity for `{stored_name}`"
                    )));
                }
                for parameter in &mut def.params {
                    if let Some(default) = &mut parameter.default {
                        migrated |= default.upgrade_legacy_serialized_dispatches();
                    }
                }
                if let FunctionBody::Statements(statements) = &mut def.body {
                    for statement in statements {
                        migrated |= statement.upgrade_legacy_serialized_dispatches();
                    }
                }
                let definition_relation =
                    RelationIdentity::from_legacy_name(&def.name).map_err(|error| {
                        StorageBackendError::Other(format!(
                            "invalid persisted routine definition name `{}`: {error}",
                            def.name
                        ))
                    })?;
                if definition_relation != stored_relation {
                    return Err(StorageBackendError::Other(format!(
                        "persisted routine registry key `{stored_name}` does not match definition `{}`",
                        def.name
                    )));
                }
                def.name.clone_from(&canonical_name);
                let signature = routine_signature_types(&def);
                let definitions = canonical_defs.entry(canonical_name.clone()).or_default();
                if definitions
                    .iter()
                    .any(|existing| routine_signature_types(existing) == signature)
                {
                    return Err(StorageBackendError::Other(format!(
                        "duplicate persisted routine identity `{}`",
                        routine_signature_label(&canonical_name, &signature)
                    )));
                }
                definitions.push(def);
            }
        }
        Ok((canonical_defs, migrated))
    }

    pub(crate) fn install_sql_function_restore_placeholders(
        &self,
        catalog: &dyn CatalogFacade,
        mode: CatalogRestoreMode,
    ) -> StorageBackendResult<Option<PendingSQLFunctionRestore>> {
        let Some(json) = catalog.get_metadata(FUNCTIONS_METADATA_KEY)? else {
            return Ok(None);
        };
        let defs = serde_json::from_str::<BTreeMap<String, Vec<CreateFunction>>>(&json)?;
        let (canonical_defs, migrated) = self.canonicalize_persisted_sql_functions(defs)?;
        if migrated && !mode.allows_migration() {
            return Err(StorageBackendError::Other(
                "routine catalog requires an initial-open object-identity migration".into(),
            ));
        }

        // Install definition-only placeholders before compiling stored SQL-standard bodies so every exact routine identity is visible while durable function bindings are rebuilt. No routine can execute during engine construction, and any compile failure restores the previous registry atomically.
        let placeholders = canonical_defs
            .iter()
            .map(|(name, definitions)| {
                let mut overloads = definitions
                    .iter()
                    .cloned()
                    .map(|def| {
                        Arc::new(SQLUserFunction {
                            def,
                            compiled: CompiledFunctionBody::SQL(Vec::new()),
                        })
                    })
                    .collect::<Vec<_>>();
                overloads.sort_by(|left, right| {
                    routine_signature_types(&left.def)
                        .cmp(&routine_signature_types(&right.def))
                        .then_with(|| left.def.is_procedure.cmp(&right.def.is_procedure))
                });
                (name.clone(), overloads)
            })
            .collect();
        let previous =
            std::mem::replace(&mut *self.durable.sql_user_functions.write(), placeholders);
        Ok(Some(PendingSQLFunctionRestore {
            definitions: canonical_defs,
            migrated,
            previous,
        }))
    }

    pub(crate) fn finalize_sql_function_restore(
        &self,
        pending: PendingSQLFunctionRestore,
        mode: CatalogRestoreMode,
    ) -> StorageBackendResult<()> {
        let PendingSQLFunctionRestore {
            definitions,
            mut migrated,
            previous,
        } = pending;
        let compiled = (|| {
            let mut restored: BTreeMap<String, Vec<Arc<SQLUserFunction>>> = BTreeMap::new();
            for (name, definitions) in definitions {
                let mut overloads = Vec::with_capacity(definitions.len());
                for mut def in definitions {
                    let (compiled, definition_migrated) = self
                        .compile_catalog_bound_routine(&mut def, RoutineCompilationMode::Persisted)
                        .map_err(|err| StorageBackendError::Other(err.to_string()))?;
                    migrated |= definition_migrated;
                    overloads.push(Arc::new(SQLUserFunction { def, compiled }));
                }
                overloads.sort_by(|left, right| {
                    routine_signature_types(&left.def)
                        .cmp(&routine_signature_types(&right.def))
                        .then_with(|| left.def.is_procedure.cmp(&right.def.is_procedure))
                });
                restored.insert(name, overloads);
            }
            Ok(restored)
        })();
        let restored = match compiled {
            Ok(restored) => restored,
            Err(error) => {
                *self.durable.sql_user_functions.write() = previous;
                return Err(error);
            }
        };
        if migrated && !mode.allows_migration() {
            *self.durable.sql_user_functions.write() = previous;
            return Err(StorageBackendError::Other(
                "routine-owned dependency bindings require an initial-open object-identity migration"
                    .into(),
            ));
        }
        if migrated {
            if let Err(error) = self.persist_sql_functions_snapshot(&restored) {
                *self.durable.sql_user_functions.write() = previous;
                return Err(StorageBackendError::Other(error.to_string()));
            }
        }
        *self.durable.sql_user_functions.write() = restored;
        Ok(())
    }
}

/// `CREATE OR REPLACE` may not change the declared result shape.
#[cfg(test)]
mod tests {
    use std::sync::{mpsc, Arc};

    use uqa_sql::ast::{DropFunctionStmt, Statement};

    use super::*;
    use crate::user_functions::canonical_routine_type_name;

    fn create_function(sql: &str) -> CreateFunction {
        let mut statements = uqa_sql::compile(sql).expect("compile CREATE FUNCTION");
        assert_eq!(statements.len(), 1);
        let Statement::CreateFunction(definition) = statements.remove(0) else {
            panic!("expected CREATE FUNCTION statement");
        };
        *definition
    }

    fn drop_function(sql: &str) -> DropFunctionStmt {
        let mut statements = uqa_sql::compile(sql).expect("compile DROP FUNCTION");
        assert_eq!(statements.len(), 1);
        let Statement::DropFunction(statement) = statements.remove(0) else {
            panic!("expected DROP FUNCTION statement");
        };
        statement
    }

    fn has_function(engine: &Engine, name: &str, argument_types: &[&str]) -> bool {
        let expected = argument_types
            .iter()
            .map(|type_name| canonical_routine_type_name(type_name))
            .collect::<Vec<_>>();
        engine
            .durable
            .sql_user_functions
            .read()
            .get(name)
            .is_some_and(|overloads| {
                overloads
                    .iter()
                    .any(|function| routine_signature_types(&function.def) == expected)
            })
    }

    #[test]
    fn drop_preserves_registration_completed_after_dependency_preflight() {
        let engine = Arc::new(Engine::new());
        engine
            .register_sql_function(create_function(
                "CREATE FUNCTION public.drop_target() RETURNS INTEGER LANGUAGE SQL IMMUTABLE AS 'SELECT 1'",
            ))
            .unwrap();
        let drop_statement = drop_function("DROP FUNCTION public.drop_target()");
        let (preflight_complete_tx, preflight_complete_rx) = mpsc::sync_channel(0);
        let (continue_tx, continue_rx) = mpsc::sync_channel(0);
        let drop_engine = Arc::clone(&engine);
        let drop_thread = std::thread::spawn(move || {
            let plan = drop_engine
                .preflight_sql_function_drop(&drop_statement)
                .unwrap();
            preflight_complete_tx.send(()).unwrap();
            continue_rx.recv().unwrap();
            drop_engine.commit_sql_function_drop(plan)
        });

        preflight_complete_rx.recv().unwrap();
        engine
            .register_sql_function(create_function(
                "CREATE FUNCTION public.drop_target(value INTEGER) RETURNS INTEGER LANGUAGE SQL IMMUTABLE AS 'SELECT $1'",
            ))
            .unwrap();
        continue_tx.send(()).unwrap();
        drop_thread.join().unwrap().unwrap();

        assert!(!has_function(&engine, "public.drop_target", &[]));
        assert!(has_function(&engine, "public.drop_target", &["INTEGER"]));
    }

    #[test]
    fn multi_target_drop_revalidation_is_atomic() {
        let engine = Engine::new();
        for sql in [
            "CREATE FUNCTION public.drop_first() RETURNS INTEGER LANGUAGE SQL IMMUTABLE AS 'SELECT 1'",
            "CREATE FUNCTION public.drop_second() RETURNS INTEGER LANGUAGE SQL IMMUTABLE AS 'SELECT 2'",
        ] {
            engine
                .register_sql_function(create_function(sql))
                .unwrap();
        }
        let plan = engine
            .preflight_sql_function_drop(&drop_function(
                "DROP FUNCTION public.drop_first(), public.drop_second()",
            ))
            .unwrap();
        engine
            .drop_sql_functions(&drop_function("DROP FUNCTION public.drop_second()"))
            .unwrap();

        let error = engine.commit_sql_function_drop(plan).unwrap_err();
        assert!(matches!(error, SQLError::Internal(_)), "{error}");
        assert!(has_function(&engine, "public.drop_first", &[]));
        assert!(!has_function(&engine, "public.drop_second", &[]));
    }
}
