//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Routine registration, catalog persistence, alteration, and removal.

use std::collections::BTreeMap;

use uqa_sql::ast::{
    AlterRoutineKind, AlterRoutineStmt, CreateFunction, DropFunctionItem, DropFunctionStmt,
};
use uqa_sql::SQLError;

use crate::{
    Arc, CatalogFacade, Engine, RelationIdentity, StorageBackendError, StorageBackendResult,
    FUNCTIONS_METADATA_KEY,
};

use super::declaration::{
    compile_function_body, resolve_alter_routine_identity_types, resolve_routine_type_references,
};
use super::resolution::{routine_kind, routine_signature_types};
use super::{canonical_routine_type_name, SQLUserFunction};

struct SQLFunctionDropPlan {
    is_procedure: bool,
    kind: &'static str,
    targets: Vec<(String, Vec<String>)>,
    dependent_views: Vec<String>,
    dependent_columns: Vec<(String, String)>,
    notices: Vec<(&'static str, String)>,
}

fn routine_signature_label(name: &str, types: &[String]) -> String {
    format!("{name}({})", types.join(", "))
}

fn wrong_routine_kind_error(
    name: &str,
    types: &[String],
    actual_is_procedure: bool,
    expected_kind: &str,
) -> SQLError {
    let actual_kind = if actual_is_procedure {
        "procedure"
    } else {
        "function"
    };
    SQLError::Routine {
        sqlstate: "42809".into(),
        message: format!(
            "{} is a {actual_kind}, not a {expected_kind}",
            routine_signature_label(name, types)
        ),
    }
}

impl Engine {
    /// Register (or replace) a user-defined routine. Applies the
    /// `PostgreSQL` conflict rules for `(schema, name, argument types)`
    /// collisions and persists the updated overload set.
    pub(crate) fn register_sql_function(&self, mut def: CreateFunction) -> Result<(), SQLError> {
        let requested_name = def.name.clone();
        def.name = self
            .try_relation_name_for_create(&requested_name)
            .map_err(|error| SQLError::Routine {
                sqlstate: "3F000".into(),
                message: error,
            })?;
        resolve_routine_type_references(self, &mut def)?;
        let compiled = compile_function_body(self, &def)?;
        let name = def.name.clone();
        let signature = routine_signature_types(&def);
        let kind = routine_kind(&def);
        let mut registry = self.durable.sql_user_functions.write();
        let mut next = registry.clone();
        {
            let overloads = next.entry(name.clone()).or_default();
            if let Some(pos) = overloads
                .iter()
                .position(|function| routine_signature_types(&function.def) == signature)
            {
                let existing = &overloads[pos].def;
                if !def.or_replace {
                    return Err(SQLError::Routine {
                        sqlstate: "42723".into(),
                        message: format!(
                            "{kind} \"{requested_name}\" already exists with same argument types"
                        ),
                    });
                }
                if existing.is_procedure != def.is_procedure {
                    return Err(SQLError::Routine {
                        sqlstate: "42809".into(),
                        message: "cannot change routine kind".into(),
                    });
                }
                if !same_return_shape(existing, &def) {
                    return Err(SQLError::Routine {
                        sqlstate: "42P13".into(),
                        message: "cannot change return type of existing function".into(),
                    });
                }
                overloads[pos] = Arc::new(SQLUserFunction { def, compiled });
            } else {
                overloads.push(Arc::new(SQLUserFunction { def, compiled }));
            }
            overloads.sort_by(|left, right| {
                routine_signature_types(&left.def)
                    .cmp(&routine_signature_types(&right.def))
                    .then_with(|| left.def.is_procedure.cmp(&right.def.is_procedure))
            });
        }
        self.persist_sql_functions_snapshot(&next)?;
        *registry = next;
        drop(registry);
        self.note_catalog_registry_changed();
        Ok(())
    }

    /// Change mutable routine attributes without replacing its identity or compiled body.
    pub(crate) fn alter_sql_routine(&self, stmt: &AlterRoutineStmt) -> Result<(), SQLError> {
        let requested_types = resolve_alter_routine_identity_types(self, stmt)?;
        let mut registry = self.durable.sql_user_functions.write();
        let (name, position) = self.resolve_sql_routine_alter_target(
            &registry,
            &stmt.name,
            requested_types.as_deref(),
            stmt.kind,
        )?;
        let existing = registry
            .get(&name)
            .and_then(|overloads| overloads.get(position))
            .cloned()
            .ok_or_else(|| {
                SQLError::Internal(format!(
                    "resolved ALTER routine target `{name}` disappeared before mutation"
                ))
            })?;
        if existing.def.is_procedure && (stmt.volatility.is_some() || stmt.strict.is_some()) {
            return Err(SQLError::Routine {
                sqlstate: "42P13".into(),
                message: "invalid attribute in procedure definition".into(),
            });
        }

        let mut def = existing.def.clone();
        if let Some(volatility) = stmt.volatility {
            def.volatility = volatility;
        }
        if let Some(strict) = stmt.strict {
            def.strict = strict;
        }
        let mut next = registry.clone();
        let overloads = next.get_mut(&name).ok_or_else(|| {
            SQLError::Internal(format!(
                "resolved ALTER routine registry entry `{name}` disappeared before mutation"
            ))
        })?;
        overloads[position] = Arc::new(SQLUserFunction {
            def,
            compiled: existing.compiled.clone(),
        });
        self.persist_sql_functions_snapshot(&next)?;
        *registry = next;
        drop(registry);
        self.note_catalog_registry_changed();
        Ok(())
    }

    /// Drop routines per a `DROP FUNCTION` / `DROP PROCEDURE`
    /// statement, mirroring `PostgreSQL`'s resolution and error
    /// texts. `IF EXISTS` misses emit a notice instead of failing.
    pub(crate) fn drop_sql_functions(&self, stmt: &DropFunctionStmt) -> Result<(), SQLError> {
        self.with_implicit_transaction(|engine| {
            let plan = engine.preflight_sql_function_drop(stmt)?;
            engine.commit_sql_function_drop(plan)
        })
    }

    /// Resolve every target and dependency before acquiring the registry write lock. Dependency scans take table and view locks, so keeping them out of the registry critical section preserves the catalog lock order.
    fn preflight_sql_function_drop(
        &self,
        stmt: &DropFunctionStmt,
    ) -> Result<SQLFunctionDropPlan, SQLError> {
        let kind = if stmt.is_procedure {
            "procedure"
        } else {
            "function"
        };
        let registry = self.durable.sql_user_functions.read().clone();
        let mut targets = Vec::new();
        let mut seen_targets = std::collections::BTreeSet::new();
        let mut notices = Vec::new();
        for item in &stmt.items {
            let target =
                self.resolve_sql_function_drop_target(&registry, item, stmt.is_procedure, kind)?;
            if let Some((key, position)) = target {
                let signature = routine_signature_types(&registry[&key][position].def);
                if seen_targets.insert((key.clone(), signature.clone())) {
                    targets.push((key, signature));
                }
            } else {
                let spelled = match &item.arg_types {
                    Some(types) => format!("{}({})", item.name, types.join(", ")),
                    None => format!("{}()", item.name),
                };
                if stmt.if_exists {
                    notices.push((
                        "NOTICE",
                        format!("{kind} {spelled} does not exist, skipping"),
                    ));
                    continue;
                }
                let described = match &item.arg_types {
                    Some(_) => format!("{kind} {spelled} does not exist"),
                    None => format!("could not find a {kind} named \"{}\"", item.name),
                };
                return Err(SQLError::Routine {
                    sqlstate: "42883".into(),
                    message: described,
                });
            }
        }
        let mut dependent_views = Vec::new();
        let mut dependent_columns = Vec::new();
        if !stmt.is_procedure {
            for (name, argument_types) in &targets {
                let columns = self.generated_function_dependents(name, argument_types)?;
                let views = self
                    .views_depending_on_function(name, argument_types)
                    .map_err(|error| {
                        SQLError::Internal(format!("read view function dependencies: {error}"))
                    })?;
                if stmt.cascade {
                    dependent_columns.extend(columns);
                    dependent_views.extend(views);
                } else {
                    self.ensure_no_function_dependencies(name, argument_types)?;
                }
            }
        }
        dependent_columns.sort();
        dependent_columns.dedup();
        dependent_views = self.cascade_view_closure(dependent_views)?;
        Ok(SQLFunctionDropPlan {
            is_procedure: stmt.is_procedure,
            kind,
            targets,
            dependent_views,
            dependent_columns,
            notices,
        })
    }

    /// Apply a completed DROP preflight against the latest registry snapshot. The write guard serializes this persistence boundary with CREATE OR REPLACE, while exact target revalidation prevents partial multi-target removal if another DROP won the race.
    fn commit_sql_function_drop(&self, plan: SQLFunctionDropPlan) -> Result<(), SQLError> {
        let SQLFunctionDropPlan {
            is_procedure,
            kind,
            targets,
            dependent_views,
            dependent_columns,
            notices,
        } = plan;
        if !dependent_views.is_empty() {
            self.drop_views_inner(&dependent_views)?;
        }
        for (table, column) in &dependent_columns {
            if !self.try_drop_column_inner(table, column).map_err(|error| {
                SQLError::Internal(format!(
                    "drop generated column `{table}`.`{column}` while cascading routine: {error}"
                ))
            })? {
                return Err(SQLError::Internal(format!(
                    "generated column `{table}`.`{column}` disappeared after routine DROP preflight"
                )));
            }
        }
        if !targets.is_empty() {
            let mut registry = self.durable.sql_user_functions.write();
            let mut next = registry.clone();

            // Revalidate every target before mutating `next`. This retains a concurrently registered unrelated overload and keeps a multi-target DROP all-or-nothing if any preflighted identity has disappeared.
            for (name, argument_types) in &targets {
                let overloads = next.get(name).ok_or_else(|| {
                    SQLError::Internal(format!(
                        "resolved {kind} registry entry `{name}` disappeared before DROP"
                    ))
                })?;
                if !overloads.iter().any(|function| {
                    function.def.is_procedure == is_procedure
                        && routine_signature_types(&function.def) == *argument_types
                }) {
                    return Err(SQLError::Internal(format!(
                        "resolved {} disappeared before DROP",
                        routine_signature_label(name, argument_types)
                    )));
                }
            }

            for (name, argument_types) in &targets {
                let overloads = next.get_mut(name).ok_or_else(|| {
                    SQLError::Internal(format!(
                        "resolved {kind} registry entry `{name}` disappeared while applying DROP"
                    ))
                })?;
                let position = overloads
                    .iter()
                    .position(|function| {
                        function.def.is_procedure == is_procedure
                            && routine_signature_types(&function.def) == *argument_types
                    })
                    .ok_or_else(|| {
                        SQLError::Internal(format!(
                            "resolved {} disappeared while applying DROP",
                            routine_signature_label(name, argument_types)
                        ))
                    })?;
                overloads.remove(position);
                if overloads.is_empty() {
                    next.remove(name);
                }
            }
            self.persist_sql_functions_snapshot(&next)?;
            *registry = next;
            drop(registry);
            self.note_catalog_registry_changed();
        }
        for (level, message) in notices {
            self.push_sql_notice(level, &message);
        }
        Ok(())
    }

    fn generated_function_dependents(
        &self,
        name: &str,
        argument_types: &[String],
    ) -> Result<Vec<(String, String)>, SQLError> {
        let mut dependents = Vec::new();
        for table in self.table_names().map_err(|error| {
            SQLError::Internal(format!("read generated function dependencies: {error}"))
        })? {
            let columns = self
                .try_describe_table(&table)
                .map_err(|error| {
                    SQLError::Internal(format!("read generated function dependencies: {error}"))
                })?
                .ok_or_else(|| SQLError::UnknownTable(table.clone()))?;
            for column in columns {
                let Some(generated) = column.generated else {
                    continue;
                };
                if generated.function_dependencies.iter().any(|dependency| {
                    dependency.name == name && dependency.argument_types == argument_types
                }) {
                    dependents.push((table.clone(), column.name));
                }
            }
        }
        dependents.sort();
        Ok(dependents)
    }

    fn cascade_view_closure(&self, initial: Vec<String>) -> Result<Vec<String>, SQLError> {
        let mut views = initial;
        views.sort();
        views.dedup();
        let mut index = 0;
        while index < views.len() {
            let dependents = self
                .views_depending_on_relation(&views[index])
                .map_err(|error| {
                    SQLError::Internal(format!("read cascading view dependencies: {error}"))
                })?;
            for dependent in dependents {
                if !views.contains(&dependent) {
                    views.push(dependent);
                }
            }
            index += 1;
        }
        views.sort();
        Ok(views)
    }

    fn ensure_no_function_dependencies(
        &self,
        name: &str,
        argument_types: &[String],
    ) -> Result<(), SQLError> {
        let generated = self.generated_function_dependents(name, argument_types)?;
        let views = self
            .views_depending_on_function(name, argument_types)
            .map_err(|error| {
                SQLError::Internal(format!("read view function dependencies: {error}"))
            })?;
        if generated.is_empty() && views.is_empty() {
            return Ok(());
        }
        let mut dependency_kinds = Vec::new();
        if !generated.is_empty() {
            dependency_kinds.push(format!(
                "generated column(s) `{}`",
                generated
                    .iter()
                    .map(|(table, column)| format!("{table}.{column}"))
                    .collect::<Vec<_>>()
                    .join("`, `")
            ));
        }
        if !views.is_empty() {
            dependency_kinds.push(format!("view(s) `{}`", views.join("`, `")));
        }
        Err(SQLError::Routine {
            sqlstate: "2BP01".into(),
            message: format!(
                "cannot drop function {} because {} depend on it",
                routine_signature_label(name, argument_types),
                dependency_kinds.join(" and ")
            ),
        })
    }

    fn resolve_sql_function_drop_target(
        &self,
        registry: &BTreeMap<String, Vec<Arc<SQLUserFunction>>>,
        item: &DropFunctionItem,
        is_procedure: bool,
        expected_kind: &str,
    ) -> Result<Option<(String, usize)>, SQLError> {
        let requested_types = item.arg_types.as_ref().map(|types| {
            types
                .iter()
                .map(|type_name| canonical_routine_type_name(type_name))
                .collect::<Vec<_>>()
        });
        for key in self.routine_lookup_keys(&item.name)? {
            let Some(overloads) = registry.get(&key) else {
                continue;
            };
            if let Some(types) = requested_types.as_ref() {
                let Some((position, function)) = overloads
                    .iter()
                    .enumerate()
                    .find(|(_, function)| routine_signature_types(&function.def) == *types)
                else {
                    continue;
                };
                if function.def.is_procedure != is_procedure {
                    return Err(wrong_routine_kind_error(
                        &function.def.name,
                        types,
                        function.def.is_procedure,
                        expected_kind,
                    ));
                }
                return Ok(Some((key, position)));
            }

            let positions = overloads
                .iter()
                .enumerate()
                .filter(|(_, function)| function.def.is_procedure == is_procedure)
                .map(|(position, _)| position)
                .collect::<Vec<_>>();
            match positions.as_slice() {
                [] => {
                    if let Some(function) = overloads.first() {
                        return Err(wrong_routine_kind_error(
                            &function.def.name,
                            &routine_signature_types(&function.def),
                            function.def.is_procedure,
                            expected_kind,
                        ));
                    }
                }
                [position] => return Ok(Some((key, *position))),
                _ => {
                    return Err(SQLError::Routine {
                        sqlstate: "42725".into(),
                        message: format!("{expected_kind} name \"{}\" is not unique", item.name),
                    });
                }
            }
        }
        Ok(None)
    }

    fn resolve_sql_routine_alter_target(
        &self,
        registry: &BTreeMap<String, Vec<Arc<SQLUserFunction>>>,
        requested_name: &str,
        requested_types: Option<&[String]>,
        kind: AlterRoutineKind,
    ) -> Result<(String, usize), SQLError> {
        let kind_name = alter_routine_kind_name(kind);
        let keys = self.routine_lookup_keys(requested_name)?;
        if let Some(types) = requested_types {
            for key in keys {
                let Some(overloads) = registry.get(&key) else {
                    continue;
                };
                let Some((position, function)) = overloads
                    .iter()
                    .enumerate()
                    .find(|(_, function)| routine_signature_types(&function.def) == types)
                else {
                    continue;
                };
                if !alter_routine_kind_matches(kind, &function.def) {
                    return Err(wrong_routine_kind_error(
                        &function.def.name,
                        types,
                        function.def.is_procedure,
                        kind_name,
                    ));
                }
                return Ok((key, position));
            }
            return Err(SQLError::Routine {
                sqlstate: "42883".into(),
                message: format!(
                    "{kind_name} {} does not exist",
                    routine_signature_label(requested_name, types)
                ),
            });
        }

        // PostgreSQL applies search-path shadowing by declared identity before filtering FUNCTION versus PROCEDURE. Thus an earlier procedure can hide a same-signature function in a later schema, while a distinct later function remains visible.
        let mut visible_signatures = std::collections::BTreeSet::new();
        let mut candidates = Vec::new();
        for key in keys {
            let Some(overloads) = registry.get(&key) else {
                continue;
            };
            for (position, function) in overloads.iter().enumerate() {
                let signature = routine_signature_types(&function.def);
                if visible_signatures.insert(signature)
                    && alter_routine_kind_matches(kind, &function.def)
                {
                    candidates.push((key.clone(), position));
                }
            }
        }
        match candidates.as_slice() {
            [(name, position)] => Ok((name.clone(), *position)),
            [] => Err(SQLError::Routine {
                sqlstate: "42883".into(),
                message: format!("could not find a {kind_name} named \"{requested_name}\""),
            }),
            _ => Err(SQLError::Routine {
                sqlstate: "42725".into(),
                message: format!("{kind_name} name \"{requested_name}\" is not unique"),
            }),
        }
    }

    fn routine_lookup_keys(&self, name: &str) -> Result<Vec<String>, SQLError> {
        let (schema, local_name) =
            RelationIdentity::parse_reference(name).map_err(|error| SQLError::Routine {
                sqlstate: "42602".into(),
                message: format!("invalid routine name `{name}`: {error}"),
            })?;
        if let Some(schema) = schema {
            return Ok(vec![
                RelationIdentity::new(schema, local_name).qualified_name()
            ]);
        }
        Ok(self
            .session
            .state
            .read()
            .search_path
            .iter()
            .map(|schema| RelationIdentity::new(schema, &local_name).qualified_name())
            .collect())
    }

    /// Visible overload set for `name`. Identical signatures in later
    /// `search_path` schemas are shadowed while distinct signatures remain
    /// candidates, matching `PostgreSQL`'s routine lookup rules.
    pub(crate) fn lookup_sql_functions(&self, name: &str) -> Option<Vec<Arc<SQLUserFunction>>> {
        let keys = self.routine_lookup_keys(name).ok()?;
        let registry = self.durable.sql_user_functions.read();
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
    ) -> Option<Vec<Arc<SQLUserFunction>>> {
        let keys = self.routine_lookup_keys(name).ok()?;
        let registry = self.durable.sql_user_functions.read();
        let candidates = keys
            .into_iter()
            .filter_map(|key| registry.get(&key))
            .flat_map(|overloads| overloads.iter().cloned())
            .collect::<Vec<_>>();
        (!candidates.is_empty()).then_some(candidates)
    }

    /// Every registered routine, sorted by qualified name then signature. Feeds
    /// `pg_catalog.pg_proc` / `information_schema.routines`.
    pub(crate) fn list_sql_functions(&self) -> Vec<Arc<SQLUserFunction>> {
        let registry = self.durable.sql_user_functions.read();
        let mut out: Vec<Arc<SQLUserFunction>> = Vec::new();
        for overloads in registry.values() {
            let mut sorted = overloads.clone();
            sorted.sort_by(|left, right| {
                routine_signature_types(&left.def)
                    .cmp(&routine_signature_types(&right.def))
                    .then_with(|| left.def.is_procedure.cmp(&right.def.is_procedure))
            });
            out.extend(sorted);
        }
        out
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
        self.runtime
            .notices
            .lock()
            .push((level.to_string(), message.to_string()));
    }

    /// Drain queued notices as `(level, message)` pairs in emission
    /// order.
    pub fn take_sql_notices(&self) -> Vec<(String, String)> {
        std::mem::take(&mut *self.runtime.notices.lock())
    }

    fn persist_sql_functions_snapshot(
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

    pub(crate) fn restore_sql_functions_from_metadata(
        &self,
        catalog: &dyn CatalogFacade,
    ) -> StorageBackendResult<()> {
        let Some(json) = catalog.get_metadata(FUNCTIONS_METADATA_KEY)? else {
            return Ok(());
        };
        let defs = serde_json::from_str::<BTreeMap<String, Vec<CreateFunction>>>(&json)?;
        let mut restored: BTreeMap<String, Vec<Arc<SQLUserFunction>>> = BTreeMap::new();
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
                .contains(&stored_relation.schema)
            {
                return Err(StorageBackendError::Other(format!(
                    "persisted routine `{stored_name}` references missing schema `{}`",
                    stored_relation.schema
                )));
            }
            let canonical_name = stored_relation.qualified_name();
            for mut def in overloads {
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
                let compiled_overloads = restored.entry(canonical_name.clone()).or_default();
                if compiled_overloads
                    .iter()
                    .any(|function| routine_signature_types(&function.def) == signature)
                {
                    return Err(StorageBackendError::Other(format!(
                        "duplicate persisted routine identity `{}`",
                        routine_signature_label(&canonical_name, &signature)
                    )));
                }
                let compiled = compile_function_body(self, &def)
                    .map_err(|err| StorageBackendError::Other(err.to_string()))?;
                compiled_overloads.push(Arc::new(SQLUserFunction { def, compiled }));
            }
        }
        for overloads in restored.values_mut() {
            overloads.sort_by(|left, right| {
                routine_signature_types(&left.def)
                    .cmp(&routine_signature_types(&right.def))
                    .then_with(|| left.def.is_procedure.cmp(&right.def.is_procedure))
            });
        }
        *self.durable.sql_user_functions.write() = restored;
        Ok(())
    }
}

fn alter_routine_kind_name(kind: AlterRoutineKind) -> &'static str {
    match kind {
        AlterRoutineKind::Function => "function",
        AlterRoutineKind::Procedure => "procedure",
        AlterRoutineKind::Routine => "routine",
    }
}

fn alter_routine_kind_matches(kind: AlterRoutineKind, def: &CreateFunction) -> bool {
    match kind {
        AlterRoutineKind::Function => !def.is_procedure,
        AlterRoutineKind::Procedure => def.is_procedure,
        AlterRoutineKind::Routine => true,
    }
}

/// `CREATE OR REPLACE` may not change the declared result shape.
fn same_return_shape(a: &CreateFunction, b: &CreateFunction) -> bool {
    use uqa_sql::ast::FunctionReturns;
    let same_outputs = {
        let a_outs = a.output_params();
        let b_outs = b.output_params();
        a_outs.len() == b_outs.len()
            && a_outs.iter().zip(&b_outs).all(|(x, y)| {
                x.name == y.name
                    && canonical_routine_type_name(&x.type_name)
                        == canonical_routine_type_name(&y.type_name)
                    && x.mode == y.mode
            })
    };
    let same_kind = match (&a.returns, &b.returns) {
        (FunctionReturns::None, FunctionReturns::None)
        | (FunctionReturns::Table, FunctionReturns::Table) => true,
        (FunctionReturns::Scalar { type_name: x }, FunctionReturns::Scalar { type_name: y })
        | (FunctionReturns::SetOf { type_name: x }, FunctionReturns::SetOf { type_name: y }) => {
            canonical_routine_type_name(x) == canonical_routine_type_name(y)
        }
        _ => false,
    };
    same_kind && same_outputs
}

#[cfg(test)]
mod tests {
    use std::sync::{mpsc, Arc};

    use uqa_sql::ast::Statement;

    use super::*;

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
