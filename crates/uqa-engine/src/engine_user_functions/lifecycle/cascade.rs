//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Joint routine and domain dependency closure for cascading object deletion.

use super::{
    routine_signature_types, Arc, BTreeMap, BTreeSet, Engine, RoutineDropResolution,
    RoutineDropTarget, SQLError, SQLFunctionDropPlan, SQLUserFunction,
};

impl Engine {
    pub(crate) fn drop_domain_types_and_routines(
        &self,
        targets: &BTreeSet<u32>,
        cascade: bool,
    ) -> Result<(), SQLError> {
        if targets.is_empty() {
            return Ok(());
        }
        let registry = self.durable.sql_user_functions.read().clone();
        let mut resolution = RoutineDropResolution {
            targets: Vec::new(),
            seen_targets: BTreeSet::new(),
            notices: Vec::new(),
        };
        let mut domains = targets.clone();
        self.expand_routine_domain_drop(&registry, &mut resolution, &mut domains)?;
        if !cascade
            && (domains != *targets
                || !resolution.targets.is_empty()
                || self.domain_drop_has_dependents(targets)?)
        {
            let message = if targets.len() == 1 {
                let oid = *targets.first().expect("one root domain");
                let name = crate::sql::resolve_regtype_output(
                    self,
                    &uqa_sql::ast::ColumnType::Regtype,
                    i64::from(oid),
                )
                .map_err(SQLError::Internal)?
                .ok_or_else(|| SQLError::Internal("DROP DOMAIN target disappeared".into()))?;
                format!("cannot drop type {name} because other objects depend on it")
            } else {
                "cannot drop desired object(s) because other objects depend on them".into()
            };
            return Err(SQLError::Routine {
                sqlstate: "2BP01".into(),
                message,
            });
        }
        let dependents = self.routine_object_dependents(&resolution.targets, true)?;
        self.commit_sql_function_drop(SQLFunctionDropPlan {
            domains,
            targets: resolution.targets,
            dependents,
            notices: resolution.notices,
        })
    }

    /// Namespace ownership authorizes contained objects without requiring ownership of their dependents.
    pub(crate) fn drop_schema_types_and_routines(
        &self,
        schemas: &BTreeSet<String>,
    ) -> Result<(), SQLError> {
        let registry = self.durable.sql_user_functions.read().clone();
        let mut resolution = RoutineDropResolution {
            targets: Vec::new(),
            seen_targets: BTreeSet::new(),
            notices: Vec::new(),
        };
        for (name, overloads) in &registry {
            for function in overloads {
                let identity =
                    crate::RelationIdentity::from_legacy_name(name).map_err(SQLError::Internal)?;
                let mut depends = schemas.contains(&identity.schema);
                if let uqa_sql::ast::FunctionBody::Statements(statements) = &function.def.body {
                    for statement in statements {
                        for relation in
                            crate::engine_events::stored_statement_relation_names(statement)?
                        {
                            let relation = crate::RelationIdentity::from_legacy_name(&relation)
                                .map_err(SQLError::Internal)?;
                            depends |= schemas.contains(&relation.schema);
                        }
                    }
                }
                if depends {
                    let target = RoutineDropTarget {
                        object_id: function.def.object_id,
                        name: name.clone(),
                        argument_types: routine_signature_types(&function.def),
                        is_procedure: function.def.is_procedure,
                    };
                    resolution.seen_targets.insert(target.clone());
                    resolution.targets.push(target);
                }
            }
        }
        let mut domains = self
            .durable
            .domains
            .read()
            .values()
            .filter(|domain| schemas.contains(&domain.identity.schema))
            .map(|domain| domain.oid)
            .collect();
        self.expand_routine_domain_drop(&registry, &mut resolution, &mut domains)?;
        let dependents = self.routine_object_dependents(&resolution.targets, true)?;
        self.commit_sql_function_drop(SQLFunctionDropPlan {
            domains,
            targets: resolution.targets,
            dependents,
            notices: resolution.notices,
        })
    }

    pub(super) fn expand_routine_domain_drop(
        &self,
        registry: &BTreeMap<String, Vec<Arc<SQLUserFunction>>>,
        resolution: &mut RoutineDropResolution,
        domains: &mut BTreeSet<u32>,
    ) -> Result<(), SQLError> {
        self.expand_routine_domain_column_drop(registry, resolution, domains, BTreeSet::new())
    }

    pub(super) fn expand_routine_domain_column_drop(
        &self,
        registry: &BTreeMap<String, Vec<Arc<SQLUserFunction>>>,
        resolution: &mut RoutineDropResolution,
        domains: &mut BTreeSet<u32>,
        mut columns: BTreeSet<(String, String)>,
    ) -> Result<(), SQLError> {
        let mut relations = BTreeSet::new();
        loop {
            let previous = (
                resolution.targets.len(),
                domains.len(),
                relations.len(),
                columns.len(),
            );
            self.expand_stored_routine_drop_dependents(registry, true, resolution)?;
            let bindings = resolution
                .targets
                .iter()
                .map(RoutineDropTarget::binding)
                .collect::<Vec<_>>();
            self.expand_domain_drop_targets(domains, &bindings)?;
            let dependents = self.routine_object_dependents(&resolution.targets, true)?;
            columns.extend(self.domain_drop_column_names(domains)?);
            columns.extend(
                dependents
                    .columns
                    .into_iter()
                    .map(|(table, column, _)| (table, column)),
            );
            relations.extend(dependents.views);
            relations.extend(self.domain_drop_view_names(domains)?);
            self.expand_column_drop_dependencies(&mut columns, &mut relations)?;
            relations = self.relation_drop_closure(relations)?;
            columns.extend(self.sequence_drop_column_names(&relations)?);
            for (name, overloads) in registry {
                for function in overloads {
                    if self.routine_references_domain(&function.def, domains)?
                        || self.stored_routine_references_relations(&function.def, &relations)?
                        || self.stored_routine_references_columns(&function.def, &columns)?
                    {
                        let target = RoutineDropTarget {
                            object_id: function.def.object_id,
                            name: name.clone(),
                            argument_types: routine_signature_types(&function.def),
                            is_procedure: function.def.is_procedure,
                        };
                        if resolution.seen_targets.insert(target.clone()) {
                            resolution.targets.push(target);
                        }
                    }
                }
            }
            if previous
                == (
                    resolution.targets.len(),
                    domains.len(),
                    relations.len(),
                    columns.len(),
                )
            {
                break;
            }
        }
        Ok(())
    }
}
