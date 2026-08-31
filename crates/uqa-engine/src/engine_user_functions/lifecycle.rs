//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Routine registration, catalog persistence, alteration, and removal.

use std::collections::{BTreeMap, BTreeSet};

use uqa_sql::ast::{
    AlterRoutineKind, AlterRoutineStmt, CreateFunction, DropFunctionItem, DropFunctionStmt,
    FunctionBinding, FunctionBody, RoleAttribute,
};
use uqa_sql::SQLError;

use crate::{
    engine_roles::role_inherits, Arc, CatalogFacade, Engine, RelationIdentity, StorageBackendError,
    StorageBackendResult, FUNCTIONS_METADATA_KEY,
};

use super::declaration::{
    compile_function_body, compile_persisted_function_body, resolve_alter_routine_identity_types,
    resolve_routine_type_references,
};
use super::resolution::{routine_kind, routine_signature_types};
use super::{canonical_routine_type_name, CompiledFunctionBody, SQLUserFunction};

struct SQLFunctionDropPlan {
    targets: Vec<RoutineDropTarget>,
    dependent_views: Vec<String>,
    dependent_columns: Vec<(String, String)>,
    dependent_triggers: Vec<(String, String)>,
    notices: Vec<(&'static str, String)>,
}

struct RoutineDropResolution {
    targets: Vec<RoutineDropTarget>,
    seen_targets: BTreeSet<RoutineDropTarget>,
    notices: Vec<(&'static str, String)>,
}

struct RoutineObjectDependents {
    views: Vec<String>,
    columns: Vec<(String, String)>,
    triggers: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct RoutineDropTarget {
    name: String,
    argument_types: Vec<String>,
    is_procedure: bool,
}

impl RoutineDropTarget {
    fn kind(&self) -> &'static str {
        if self.is_procedure {
            "procedure"
        } else {
            "function"
        }
    }

    fn label(&self) -> String {
        routine_signature_label(&self.name, &self.argument_types)
    }

    fn binding(&self) -> FunctionBinding {
        FunctionBinding {
            name: self.name.clone(),
            argument_types: self.argument_types.clone(),
            builtin: false,
            dispatch: None,
            invocation: None,
            resolution_error: None,
        }
    }
}

pub(super) fn routine_signature_label(name: &str, types: &[String]) -> String {
    let display_types = types
        .iter()
        .map(|type_name| {
            uqa_sql::ast::ColumnType::from_sql_name(type_name)
                .map_or_else(|_| type_name.clone(), |column_type| column_type.sql_name())
        })
        .collect::<Vec<_>>();
    format!("{name}({})", display_types.join(", "))
}

fn sql_standard_routine_dependents(
    registry: &BTreeMap<String, Vec<Arc<SQLUserFunction>>>,
    target: &RoutineDropTarget,
) -> Vec<RoutineDropTarget> {
    let binding = target.binding();
    let mut dependents = registry
        .iter()
        .flat_map(|(name, overloads)| {
            let binding = &binding;
            overloads.iter().filter_map(move |function| {
                if !matches!(function.def.body, FunctionBody::Statements(_)) {
                    return None;
                }
                let CompiledFunctionBody::SQL(plans) = &function.compiled else {
                    return None;
                };
                let references_target = plans.iter().any(|plan| match plan {
                    uqa_planner::UnifiedPlan::Query(query) => {
                        crate::engine_session::query_plan_references_function(query, binding)
                    }
                    uqa_planner::UnifiedPlan::Command(_) => false,
                });
                references_target.then(|| RoutineDropTarget {
                    name: name.clone(),
                    argument_types: routine_signature_types(&function.def),
                    is_procedure: function.def.is_procedure,
                })
            })
        })
        .filter(|dependent| dependent != target)
        .collect::<Vec<_>>();
    dependents.sort();
    dependents.dedup();
    dependents
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

fn append_routine_cascade_notice(
    notices: &mut Vec<(&'static str, String)>,
    cascaded_routines: &[RoutineDropTarget],
    dependents: &RoutineObjectDependents,
) {
    let mut cascaded = cascaded_routines
        .iter()
        .map(|target| format!("{} {}", target.kind(), target.label()))
        .collect::<Vec<_>>();
    cascaded.extend(
        dependents
            .columns
            .iter()
            .map(|(table, column)| format!("column {column} of table {table}")),
    );
    cascaded.extend(dependents.views.iter().map(|view| format!("view {view}")));
    cascaded.extend(
        dependents
            .triggers
            .iter()
            .map(|(table, trigger)| format!("trigger {trigger} on table {table}")),
    );
    cascaded.sort();
    cascaded.dedup();
    match cascaded.as_slice() {
        [] => {}
        [object] => notices.push(("NOTICE", format!("drop cascades to {object}"))),
        objects => notices.push((
            "NOTICE",
            format!("drop cascades to {} other objects", objects.len()),
        )),
    }
}

impl Engine {
    /// Register (or replace) a user-defined routine. Applies the
    /// `PostgreSQL` conflict rules for `(schema, name, argument types)`
    /// collisions and persists the updated overload set.
    pub(crate) fn register_sql_function(&self, mut def: CreateFunction) -> Result<(), SQLError> {
        self.prepare_explicit_transaction_writer()?;
        let requested_name = def.name.clone();
        def.name = self
            .try_relation_name_for_create(&requested_name)
            .map_err(|error| SQLError::Routine {
                sqlstate: "3F000".into(),
                message: error,
            })?;
        resolve_routine_type_references(self, &mut def)?;
        if def.owner.is_empty() {
            def.owner = self.current_user_name();
        }
        if let Some(support) = def.support.as_deref() {
            self.validate_routine_support(support)?;
        }
        self.apply_routine_config_actions(&mut def)?;
        if matches!(def.body, FunctionBody::Statements(_)) {
            def.creation_search_path
                .clone_from(&self.session.state.read().search_path);
        } else {
            def.creation_search_path.clear();
        }
        let compiled = compile_function_body(self, &def)?;
        let name = def.name.clone();
        let signature = routine_signature_types(&def);
        let kind = routine_kind(&def);
        let current_user = self.current_user_name();
        let roles = self.durable.roles.read();
        if !roles.contains_key(&def.owner) {
            return Err(SQLError::Routine {
                sqlstate: "42704".into(),
                message: format!("role \"{}\" does not exist", def.owner),
            });
        }
        let current_user_is_superuser = roles
            .get(&current_user)
            .is_some_and(|role| role.has(RoleAttribute::Superuser));
        let memberships = self.durable.role_memberships.read();
        if (def.security.leakproof || def.support.is_some()) && !current_user_is_superuser {
            return Err(SQLError::Routine {
                sqlstate: "42501".into(),
                message: if def.security.leakproof {
                    "only superuser can define a leakproof function".into()
                } else {
                    "must be superuser to specify a support function".into()
                },
            });
        }
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
                Self::ensure_routine_owner_as(
                    existing,
                    role_inherits(&roles, &memberships, &current_user, &existing.owner),
                )?;
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
                // CREATE OR REPLACE changes the definition but not object ownership or privileges.
                def.owner.clone_from(&existing.owner);
                def.execute_acl.clone_from(&existing.execute_acl);
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
        drop(memberships);
        drop(roles);
        self.note_catalog_registry_changed();
        Ok(())
    }

    /// Change mutable routine attributes without replacing its identity or compiled body.
    pub(crate) fn alter_sql_routine(&self, stmt: &AlterRoutineStmt) -> Result<(), SQLError> {
        self.prepare_explicit_transaction_writer()?;
        let requested_types = resolve_alter_routine_identity_types(self, stmt)?;
        let current_user = self.current_user_name();
        let roles = self.durable.roles.read();
        let current_user_is_superuser = roles
            .get(&current_user)
            .is_some_and(|role| role.has(RoleAttribute::Superuser));
        let memberships = self.durable.role_memberships.read();
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
        Self::ensure_routine_owner_as(
            &existing.def,
            role_inherits(&roles, &memberships, &current_user, &existing.def.owner),
        )?;
        if existing.def.is_procedure
            && (stmt.volatility.is_some()
                || stmt.strict.is_some()
                || stmt.leakproof.is_some()
                || stmt.parallel.is_some()
                || stmt.support.is_some())
        {
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
        if let Some(security_definer) = stmt.security_definer {
            def.security.security_definer = security_definer;
        }
        if let Some(leakproof) = stmt.leakproof {
            if leakproof && !current_user_is_superuser {
                return Err(SQLError::Routine {
                    sqlstate: "42501".into(),
                    message: "only superuser can define a leakproof function".into(),
                });
            }
            def.security.leakproof = leakproof;
        }
        if let Some(parallel) = stmt.parallel {
            def.parallel = parallel;
        }
        if let Some(support) = &stmt.support {
            self.validate_routine_support(support)?;
            def.support = Some(support.clone());
        }
        def.config_actions.clone_from(&stmt.config_actions);
        self.apply_routine_config_actions(&mut def)?;
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
        drop(memberships);
        drop(roles);
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
        let mut resolution = self.resolve_sql_function_drop_targets(stmt, &registry, kind)?;
        self.ensure_routine_drop_owners(&registry, &resolution.targets)?;
        let cascaded_routines =
            Self::expand_sql_standard_drop_dependents(&registry, stmt.cascade, &mut resolution)?;
        let dependents = self.routine_object_dependents(&resolution.targets, stmt.cascade)?;
        if stmt.cascade {
            append_routine_cascade_notice(&mut resolution.notices, &cascaded_routines, &dependents);
        }
        Ok(SQLFunctionDropPlan {
            targets: resolution.targets,
            dependent_views: dependents.views,
            dependent_columns: dependents.columns,
            dependent_triggers: dependents.triggers,
            notices: resolution.notices,
        })
    }

    fn ensure_routine_drop_owners(
        &self,
        registry: &BTreeMap<String, Vec<Arc<SQLUserFunction>>>,
        targets: &[RoutineDropTarget],
    ) -> Result<(), SQLError> {
        let current_user = self.current_user_name();
        let roles = self.durable.roles.read();
        let memberships = self.durable.role_memberships.read();
        for target in targets {
            let definition = registry
                .get(&target.name)
                .and_then(|overloads| {
                    overloads.iter().find(|function| {
                        function.def.is_procedure == target.is_procedure
                            && routine_signature_types(&function.def) == target.argument_types
                    })
                })
                .map(|function| &function.def)
                .ok_or_else(|| {
                    SQLError::Internal(format!(
                        "resolved {} {} disappeared before ownership validation",
                        target.kind(),
                        target.label()
                    ))
                })?;
            Self::ensure_routine_owner_as(
                definition,
                role_inherits(&roles, &memberships, &current_user, &definition.owner),
            )?;
        }
        Ok(())
    }

    fn resolve_sql_function_drop_targets(
        &self,
        stmt: &DropFunctionStmt,
        registry: &BTreeMap<String, Vec<Arc<SQLUserFunction>>>,
        kind: &'static str,
    ) -> Result<RoutineDropResolution, SQLError> {
        let mut resolution = RoutineDropResolution {
            targets: Vec::new(),
            seen_targets: BTreeSet::new(),
            notices: Vec::new(),
        };
        for item in &stmt.items {
            let target =
                self.resolve_sql_function_drop_target(registry, item, stmt.is_procedure, kind)?;
            if let Some((key, position)) = target {
                let function = &registry[&key][position];
                let target = RoutineDropTarget {
                    name: key,
                    argument_types: routine_signature_types(&function.def),
                    is_procedure: function.def.is_procedure,
                };
                if resolution.seen_targets.insert(target.clone()) {
                    resolution.targets.push(target);
                }
            } else {
                let spelled = match &item.arg_types {
                    Some(types) => format!("{}({})", item.name, types.join(", ")),
                    None => format!("{}()", item.name),
                };
                if stmt.if_exists {
                    resolution.notices.push((
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
        Ok(resolution)
    }

    fn expand_sql_standard_drop_dependents(
        registry: &BTreeMap<String, Vec<Arc<SQLUserFunction>>>,
        cascade: bool,
        resolution: &mut RoutineDropResolution,
    ) -> Result<Vec<RoutineDropTarget>, SQLError> {
        let explicit_targets = resolution.seen_targets.clone();
        let mut cascaded_routines = Vec::new();
        let mut target_index = 0;
        while target_index < resolution.targets.len() {
            let target = resolution.targets[target_index].clone();
            target_index += 1;
            if target.is_procedure {
                continue;
            }
            for dependent in sql_standard_routine_dependents(registry, &target) {
                if explicit_targets.contains(&dependent)
                    || resolution.seen_targets.contains(&dependent)
                {
                    continue;
                }
                if !cascade {
                    return Err(SQLError::Routine {
                        sqlstate: "2BP01".into(),
                        message: format!(
                            "cannot drop function {} because other objects depend on it",
                            target.label()
                        ),
                    });
                }
                resolution.seen_targets.insert(dependent.clone());
                cascaded_routines.push(dependent.clone());
                resolution.targets.push(dependent);
            }
        }
        Ok(cascaded_routines)
    }

    fn routine_object_dependents(
        &self,
        targets: &[RoutineDropTarget],
        cascade: bool,
    ) -> Result<RoutineObjectDependents, SQLError> {
        let mut dependent_views = Vec::new();
        let mut dependent_columns = Vec::new();
        let mut dependent_triggers = Vec::new();
        for target in targets {
            if !target.is_procedure {
                let columns =
                    self.generated_function_dependents(&target.name, &target.argument_types)?;
                let views = self
                    .views_depending_on_function(&target.name, &target.argument_types)
                    .map_err(|error| {
                        SQLError::Internal(format!("read view function dependencies: {error}"))
                    })?;
                let triggers = if target.argument_types.is_empty() {
                    self.list_triggers()
                        .into_iter()
                        .filter(|trigger| trigger.definition.function == target.name)
                        .map(|trigger| {
                            (
                                trigger.definition.table.clone(),
                                trigger.definition.name.clone(),
                            )
                        })
                        .collect::<Vec<_>>()
                } else {
                    Vec::new()
                };
                if cascade {
                    dependent_columns.extend(columns);
                    dependent_views.extend(views);
                    dependent_triggers.extend(triggers);
                } else {
                    Self::ensure_no_function_dependencies(
                        &target.name,
                        &target.argument_types,
                        &columns,
                        &views,
                        &triggers,
                    )?;
                }
            }
        }
        dependent_columns.sort();
        dependent_columns.dedup();
        dependent_views = self.cascade_view_closure(dependent_views)?;
        dependent_triggers.sort();
        dependent_triggers.dedup();
        Ok(RoutineObjectDependents {
            views: dependent_views,
            columns: dependent_columns,
            triggers: dependent_triggers,
        })
    }

    /// Apply a completed DROP preflight against the latest registry snapshot. The write guard serializes this persistence boundary with CREATE OR REPLACE, while exact target revalidation prevents partial multi-target removal if another DROP won the race.
    fn commit_sql_function_drop(&self, plan: SQLFunctionDropPlan) -> Result<(), SQLError> {
        let SQLFunctionDropPlan {
            targets,
            dependent_views,
            dependent_columns,
            dependent_triggers,
            notices,
        } = plan;
        for (table, name) in &dependent_triggers {
            self.drop_trigger(&uqa_sql::ast::DropTrigger {
                name: name.clone(),
                table: table.clone(),
                if_exists: false,
                cascade: true,
            })?;
        }
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
            for target in &targets {
                let overloads = next.get(&target.name).ok_or_else(|| {
                    SQLError::Internal(format!(
                        "resolved {} registry entry `{}` disappeared before DROP",
                        target.kind(),
                        target.name
                    ))
                })?;
                if !overloads.iter().any(|function| {
                    function.def.is_procedure == target.is_procedure
                        && routine_signature_types(&function.def) == target.argument_types
                }) {
                    return Err(SQLError::Internal(format!(
                        "resolved {} {} disappeared before DROP",
                        target.kind(),
                        target.label()
                    )));
                }
            }

            for target in targets.iter().rev() {
                let overloads = next.get_mut(&target.name).ok_or_else(|| {
                    SQLError::Internal(format!(
                        "resolved {} registry entry `{}` disappeared while applying DROP",
                        target.kind(),
                        target.name
                    ))
                })?;
                let position = overloads
                    .iter()
                    .position(|function| {
                        function.def.is_procedure == target.is_procedure
                            && routine_signature_types(&function.def) == target.argument_types
                    })
                    .ok_or_else(|| {
                        SQLError::Internal(format!(
                            "resolved {} {} disappeared while applying DROP",
                            target.kind(),
                            target.label()
                        ))
                    })?;
                overloads.remove(position);
                if overloads.is_empty() {
                    next.remove(&target.name);
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
        name: &str,
        argument_types: &[String],
        generated: &[(String, String)],
        views: &[String],
        triggers: &[(String, String)],
    ) -> Result<(), SQLError> {
        if generated.is_empty() && views.is_empty() && triggers.is_empty() {
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
        if !triggers.is_empty() {
            dependency_kinds.push(format!(
                "trigger(s) `{}`",
                triggers
                    .iter()
                    .map(|(table, trigger)| format!("{trigger} on {table}"))
                    .collect::<Vec<_>>()
                    .join("`, `")
            ));
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

    pub(super) fn resolve_sql_routine_alter_target(
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
    ) -> Option<Vec<Arc<SQLUserFunction>>> {
        let keys = self.routine_lookup_keys(name).ok()?;
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

    pub(super) fn persist_sql_functions_snapshot(
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

    fn compile_persisted_sql_function(
        &self,
        def: &CreateFunction,
    ) -> Result<CompiledFunctionBody, SQLError> {
        if !matches!(def.body, FunctionBody::Statements(_)) || def.creation_search_path.is_empty() {
            return compile_persisted_function_body(self, def);
        }
        let previous = {
            let mut state = self.session.state.write();
            std::mem::replace(&mut state.search_path, def.creation_search_path.clone())
        };
        let compiled = compile_persisted_function_body(self, def);
        self.session.state.write().search_path = previous;
        compiled
    }

    fn canonicalize_persisted_sql_functions(
        &self,
        defs: BTreeMap<String, Vec<CreateFunction>>,
    ) -> StorageBackendResult<BTreeMap<String, Vec<CreateFunction>>> {
        let mut canonical_defs: BTreeMap<String, Vec<CreateFunction>> = BTreeMap::new();
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
                for parameter in &mut def.params {
                    if let Some(default) = &mut parameter.default {
                        default.upgrade_legacy_serialized_dispatches();
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
        Ok(canonical_defs)
    }

    pub(crate) fn restore_sql_functions_from_metadata(
        &self,
        catalog: &dyn CatalogFacade,
    ) -> StorageBackendResult<()> {
        let Some(json) = catalog.get_metadata(FUNCTIONS_METADATA_KEY)? else {
            return Ok(());
        };
        let defs = serde_json::from_str::<BTreeMap<String, Vec<CreateFunction>>>(&json)?;
        let canonical_defs = self.canonicalize_persisted_sql_functions(defs)?;

        // Install definition-only placeholders before compiling stored SQL-standard bodies so every exact routine identity is visible while durable function bindings are rebuilt. No routine can execute during engine construction, and any compile failure restores the previous registry atomically.
        let placeholders = canonical_defs
            .iter()
            .map(|(name, definitions)| {
                let overloads = definitions
                    .iter()
                    .cloned()
                    .map(|def| {
                        Arc::new(SQLUserFunction {
                            def,
                            compiled: CompiledFunctionBody::SQL(Vec::new()),
                        })
                    })
                    .collect();
                (name.clone(), overloads)
            })
            .collect();
        let previous =
            std::mem::replace(&mut *self.durable.sql_user_functions.write(), placeholders);
        let compiled = (|| {
            let mut restored: BTreeMap<String, Vec<Arc<SQLUserFunction>>> = BTreeMap::new();
            for (name, definitions) in canonical_defs {
                let mut overloads = Vec::with_capacity(definitions.len());
                for def in definitions {
                    let compiled = self
                        .compile_persisted_sql_function(&def)
                        .map_err(|err| StorageBackendError::Other(err.to_string()))?;
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
