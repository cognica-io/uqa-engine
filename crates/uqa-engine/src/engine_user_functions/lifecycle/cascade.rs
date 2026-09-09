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
        let mut relations = BTreeSet::new();
        loop {
            let previous = (resolution.targets.len(), domains.len(), relations.len());
            self.expand_stored_routine_drop_dependents(registry, true, resolution)?;
            let bindings = resolution
                .targets
                .iter()
                .map(RoutineDropTarget::binding)
                .collect::<Vec<_>>();
            self.expand_domain_drop_targets(domains, &bindings)?;
            relations.extend(
                self.routine_object_dependents(&resolution.targets, true)?
                    .views,
            );
            relations.extend(self.domain_drop_view_names(domains)?);
            relations = self.relation_drop_closure(relations)?;
            for (name, overloads) in registry {
                for function in overloads {
                    if self.routine_references_domain(&function.def, domains)?
                        || self.stored_routine_references_relations(&function.def, &relations)?
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
            if previous == (resolution.targets.len(), domains.len(), relations.len()) {
                break;
            }
        }
        Ok(())
    }
}
