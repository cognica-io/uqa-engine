//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Prepare source-alias rewrites against the old schema before publishing surviving routines.

use uqa_sql::ast::{CreateFunction, FunctionBinding, FunctionBody};

use super::{routine_signature_types, BTreeSet, Engine, SQLError};

impl Engine {
    pub(crate) fn prepare_routine_column_alias_drop(
        &self,
        mut columns: BTreeSet<(String, String)>,
        removed_routines: &[FunctionBinding],
    ) -> Result<Vec<CreateFunction>, SQLError> {
        if columns.is_empty() {
            return Ok(Vec::new());
        }
        let mut relations = BTreeSet::new();
        loop {
            let previous = columns.len();
            self.expand_column_drop_dependencies(&mut columns, &mut relations)?;
            columns.extend(self.sequence_drop_column_names(&relations)?);
            if columns.len() == previous {
                break;
            }
        }
        let dependencies = columns
            .into_iter()
            .map(|(table, column)| {
                Ok(crate::engine_events::RuleColumnDependency {
                    relation: crate::RelationIdentity::from_legacy_name(&table)
                        .map_err(SQLError::Internal)?,
                    column,
                })
            })
            .collect::<Result<BTreeSet<_>, SQLError>>()?;
        let registry = self.durable.sql_user_functions.read().clone();
        let mut definitions = Vec::new();
        for function in registry.values().flatten() {
            if removed_routines.iter().any(|target| {
                target.object_id == function.def.object_id
                    && target.name == function.def.name
                    && target.argument_types == routine_signature_types(&function.def)
            }) {
                continue;
            }
            let mut definition = function.def.clone();
            let FunctionBody::Statements(statements) = &mut definition.body else {
                continue;
            };
            let mut changed = false;
            for statement in statements {
                changed |=
                    self.remove_stored_statement_source_column_aliases(statement, &dependencies)?;
            }
            if changed {
                definitions.push(definition);
            }
        }
        Ok(definitions)
    }
}
