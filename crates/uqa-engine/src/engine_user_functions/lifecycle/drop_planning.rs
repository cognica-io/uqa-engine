//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    alter_routine_kind_matches, alter_routine_kind_name, append_routine_cascade_notice,
    canonical_routine_type_name, role_inherits, routine_signature_label, routine_signature_types,
    sql_standard_routine_dependents, wrong_routine_kind_error, AlterRoutineKind, Arc, BTreeMap,
    BTreeSet, DropFunctionItem, DropFunctionStmt, Engine, RoutineDropResolution, RoutineDropTarget,
    RoutineObjectDependents, SQLError, SQLFunctionDropPlan, SQLUserFunction,
};

impl Engine {
    pub(crate) fn drop_sql_functions(&self, stmt: &DropFunctionStmt) -> Result<(), SQLError> {
        self.with_implicit_transaction(|engine| {
            let plan = engine.preflight_sql_function_drop(stmt)?;
            engine.commit_sql_function_drop(plan)
        })
    }

    /// Resolve every target and dependency before acquiring the registry write lock. Dependency scans take table and view locks, so keeping them out of the registry critical section preserves the catalog lock order.
    pub(super) fn preflight_sql_function_drop(
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
    pub(super) fn commit_sql_function_drop(
        &self,
        plan: SQLFunctionDropPlan,
    ) -> Result<(), SQLError> {
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

    pub(in crate::engine_user_functions) fn resolve_sql_routine_alter_target(
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
}
