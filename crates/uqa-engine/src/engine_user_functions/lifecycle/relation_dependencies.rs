//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stored SQL routine dependencies on relations and early-bound sequences.

use uqa_sql::ast::{CreateFunction, FunctionBody};

use super::{
    routine_signature_types, BTreeSet, Engine, RoutineDropResolution, RoutineDropTarget, SQLError,
    SQLFunctionDropPlan,
};

impl Engine {
    pub(super) fn sequence_drop_column_names(
        &self,
        relations: &BTreeSet<String>,
    ) -> Result<BTreeSet<(String, String)>, SQLError> {
        let sequences = self
            .durable
            .sequences
            .read()
            .keys()
            .map(crate::RelationIdentity::qualified_name)
            .filter(|name| relations.contains(name))
            .collect::<Vec<_>>();
        let mut columns = BTreeSet::new();
        for sequence in sequences {
            for dependent in self
                .sequence_schema_expression_dependents(&sequence)
                .map_err(|error| {
                    SQLError::Internal(format!("inspect sequence column dependencies: {error}"))
                })?
            {
                if let crate::engine_sequences::SequenceSchemaDependent::GeneratedColumn {
                    table,
                    column,
                    ..
                } = dependent
                {
                    columns.insert((table, column));
                }
            }
        }
        Ok(columns)
    }

    pub(super) fn stored_routine_references_columns(
        &self,
        definition: &CreateFunction,
        columns: &BTreeSet<(String, String)>,
    ) -> Result<bool, SQLError> {
        let FunctionBody::Statements(statements) = &definition.body else {
            return Ok(false);
        };
        if columns.is_empty() {
            return Ok(false);
        }
        for statement in statements {
            let dependencies = self.stored_statement_column_dependencies(statement)?;
            if dependencies.iter().any(|dependency| {
                columns.contains(&(
                    dependency.relation.qualified_name(),
                    dependency.column.clone(),
                ))
            }) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub(crate) fn relation_dependents_drop_error(
        &self,
        names: &[String],
        kind: &str,
    ) -> Result<SQLError, SQLError> {
        let names = names.iter().collect::<BTreeSet<_>>();
        let message = if names.len() == 1 {
            let name = *names.first().expect("one root relation");
            let label = match crate::sql::resolve_regclass_oid(self, name)? {
                Some(oid) => crate::sql::resolve_regtype_output(
                    self,
                    &uqa_sql::ast::ColumnType::Regclass,
                    oid,
                )
                .map_err(SQLError::Internal)?
                .unwrap_or_else(|| name.clone()),
                None => name.clone(),
            };
            format!("cannot drop {kind} {label} because other objects depend on it")
        } else {
            "cannot drop desired object(s) because other objects depend on them".into()
        };
        Ok(SQLError::Routine {
            sqlstate: "2BP01".into(),
            message,
        })
    }

    pub(super) fn relation_drop_closure(
        &self,
        mut relations: BTreeSet<String>,
    ) -> Result<BTreeSet<String>, SQLError> {
        let mut owners: BTreeSet<_> = self
            .table_entries()
            .into_iter()
            .filter(|(name, _)| relations.contains(name))
            .map(|(_, table)| table.object_id())
            .collect();
        owners.extend(
            self.durable
                .foreign_tables
                .read()
                .iter()
                .filter_map(|(identity, table)| {
                    relations
                        .contains(&identity.qualified_name())
                        .then_some(table.object_id)
                }),
        );
        relations.extend(
            self.sequence_names_owned_by_tables(&owners)
                .map_err(|error| SQLError::Internal(format!("inspect owned sequences: {error}")))?,
        );
        let mut pending = relations.iter().cloned().collect::<Vec<_>>();
        while let Some(relation) = pending.pop() {
            let mut views = self
                .views_depending_on_relation(&relation)
                .map_err(|error| {
                    SQLError::Internal(format!("inspect relation dependencies: {error}"))
                })?;
            views.extend(
                self.views_depending_on_sequence(&relation)
                    .map_err(|error| {
                        SQLError::Internal(format!("inspect sequence dependencies: {error}"))
                    })?,
            );
            for view in views {
                if relations.insert(view.clone()) {
                    pending.push(view);
                }
            }
        }
        Ok(relations)
    }

    pub(super) fn stored_routine_references_relations(
        &self,
        definition: &CreateFunction,
        relations: &BTreeSet<String>,
    ) -> Result<bool, SQLError> {
        if relations.is_empty() {
            return Ok(false);
        }
        let mut oids = BTreeSet::new();
        for relation in relations {
            if let Some(oid) = crate::sql::resolve_bound_regclass_oid(self, relation)? {
                oids.insert(oid);
            }
        }
        let mut depends = false;
        let mut visit = |expression: &mut uqa_sql::ast::Expr| {
            depends |=
                super::regclass::regclass_oid(expression).is_some_and(|oid| oids.contains(&oid));
            Ok(())
        };
        for default in definition
            .params
            .iter()
            .filter_map(|parameter| parameter.default.as_ref())
        {
            crate::engine_events::visit_stored_expression(&mut default.clone(), &mut visit)?;
        }
        if let FunctionBody::Statements(statements) = &definition.body {
            for statement in statements {
                if crate::engine_events::stored_statement_relation_names(statement)?
                    .iter()
                    .any(|name| relations.contains(name))
                {
                    return Ok(true);
                }
                crate::engine_events::visit_stored_statement_expressions(
                    &mut statement.clone(),
                    &mut visit,
                )?;
            }
        }
        Ok(depends)
    }

    pub(crate) fn drop_relation_routine_dependents(
        &self,
        names: &[String],
        cascade: bool,
        kind: &str,
    ) -> Result<(), SQLError> {
        if names.is_empty() {
            return Ok(());
        }
        let registry = self.durable.sql_user_functions.read().clone();
        if registry.is_empty() {
            return Ok(());
        }
        let mut relations = names.iter().cloned().collect::<BTreeSet<_>>();
        let mut resolution = RoutineDropResolution {
            targets: Vec::new(),
            seen_targets: BTreeSet::new(),
            notices: Vec::new(),
        };
        let mut domains = BTreeSet::new();
        loop {
            let previous = (relations.len(), resolution.targets.len(), domains.len());
            relations = self.relation_drop_closure(relations)?;
            let mut columns = self.sequence_drop_column_names(&relations)?;
            self.expand_column_drop_dependencies(&mut columns, &mut relations)?;
            for (name, overloads) in &registry {
                for function in overloads {
                    if self.stored_routine_references_relations(&function.def, &relations)?
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
            if resolution.targets.is_empty() {
                return Ok(());
            }
            if !cascade {
                return Err(self.relation_dependents_drop_error(names, kind)?);
            }
            self.expand_routine_domain_column_drop(
                &registry,
                &mut resolution,
                &mut domains,
                columns,
            )?;
            let dependents = self.routine_object_dependents(&resolution.targets, true)?;
            relations.extend(dependents.views.iter().cloned());
            if previous == (relations.len(), resolution.targets.len(), domains.len()) {
                return self.commit_sql_function_drop(SQLFunctionDropPlan {
                    domains,
                    targets: resolution.targets,
                    dependents,
                    notices: resolution.notices,
                });
            }
        }
    }
}
