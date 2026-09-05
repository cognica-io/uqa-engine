//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Creation-time binding and lifecycle traversal for routine-owned routine dependencies.

use std::collections::BTreeMap;

use uqa_sql::ast::{CreateFunction, FunctionBody};
use uqa_sql::SQLError;

use crate::engine_capabilities::RelationLookupMode;
use crate::{Arc, Engine};

use super::super::declaration::compile_function_body;
use super::super::resolution::routine_signature_types;
use super::{CompiledFunctionBody, RoutineDropTarget, SQLUserFunction};

#[derive(Clone, Copy)]
pub(super) enum RoutineCompilationMode {
    Definition,
    Persisted,
}

impl Engine {
    pub(super) fn compile_catalog_bound_routine(
        &self,
        def: &mut CreateFunction,
        mode: RoutineCompilationMode,
    ) -> Result<(CompiledFunctionBody, bool), SQLError> {
        if matches!(mode, RoutineCompilationMode::Definition) {
            self.capture_routine_creation_search_path(def);
        }
        let mut changed = self.bind_routine_definition_dependencies(def, mode)?;
        let mut compiled = self.compile_routine_for_mode(def, mode)?;
        let body_changed = self.bind_sql_standard_body_routines(def, &compiled)?;
        changed |= body_changed;
        if body_changed {
            compiled = self.compile_routine_for_mode(def, mode)?;
        }
        Ok((compiled, changed))
    }

    fn capture_routine_creation_search_path(&self, def: &mut CreateFunction) {
        if matches!(def.body, FunctionBody::Statements(_))
            || def
                .params
                .iter()
                .any(|parameter| parameter.default.is_some())
        {
            def.creation_search_path
                .clone_from(&self.session.state.read().search_path);
        } else {
            def.creation_search_path.clear();
        }
    }

    fn compile_routine_for_mode(
        &self,
        def: &CreateFunction,
        mode: RoutineCompilationMode,
    ) -> Result<CompiledFunctionBody, SQLError> {
        match mode {
            RoutineCompilationMode::Definition => compile_function_body(self, def),
            RoutineCompilationMode::Persisted => self.compile_persisted_sql_function(def),
        }
    }

    fn bind_routine_definition_dependencies(
        &self,
        def: &mut CreateFunction,
        mode: RoutineCompilationMode,
    ) -> Result<bool, SQLError> {
        if def.creation_search_path.is_empty() {
            return self.bind_routine_definition_dependencies_at_current_search_path(def, mode);
        }
        let previous = {
            let mut state = self.session.state.write();
            std::mem::replace(&mut state.search_path, def.creation_search_path.clone())
        };
        let result = self.bind_routine_definition_dependencies_at_current_search_path(def, mode);
        self.session.state.write().search_path = previous;
        result
    }

    fn bind_routine_definition_dependencies_at_current_search_path(
        &self,
        def: &mut CreateFunction,
        mode: RoutineCompilationMode,
    ) -> Result<bool, SQLError> {
        let mut changed = self.bind_sql_standard_body_relations(def, mode)?;
        for parameter in &mut def.params {
            let Some(default) = &mut parameter.default else {
                continue;
            };
            let mut plan =
                uqa_planner::ExpressionPlan::lower_with(default.clone(), &|name: &str| {
                    self.has_registered_aggregate_function(name)
                });
            crate::sql::bind_catalog_expression_routines_with_outer(
                self,
                &mut plan,
                &[],
                &uqa_execution::RowSchema::default(),
            )?;
            let references = crate::sql::collect_expression_routine_references(&plan)?;
            changed |= crate::engine_events::bind_stored_expression_routines(default, &references)?;
        }
        Ok(changed)
    }

    fn bind_sql_standard_body_relations(
        &self,
        def: &mut CreateFunction,
        mode: RoutineCompilationMode,
    ) -> Result<bool, SQLError> {
        let FunctionBody::Statements(statements) = &mut def.body else {
            return Ok(false);
        };
        let mut changed = false;
        for statement in statements {
            changed |= self.bind_stored_statement_relations(
                statement,
                RelationLookupMode::Dynamic,
                matches!(mode, RoutineCompilationMode::Persisted),
                "SQL routine body",
            )?;
        }
        Ok(changed)
    }

    fn bind_sql_standard_body_routines(
        &self,
        def: &mut CreateFunction,
        compiled: &CompiledFunctionBody,
    ) -> Result<bool, SQLError> {
        let FunctionBody::Statements(statements) = &mut def.body else {
            return Ok(false);
        };
        let CompiledFunctionBody::SQL(plans) = compiled else {
            return Err(SQLError::Internal(format!(
                "SQL-standard routine `{}` did not compile to SQL plans",
                def.name
            )));
        };
        if statements.len() != plans.len() {
            return Err(SQLError::Internal(format!(
                "SQL-standard routine `{}` has {} statements but {} plans",
                def.name,
                statements.len(),
                plans.len()
            )));
        }
        let mut changed = false;
        for (statement, plan) in statements.iter_mut().zip(plans) {
            let routines = crate::sql::bind_catalog_statement_routines(self, plan)?;
            changed |= crate::engine_events::bind_stored_statement_routines(
                statement,
                &routines.references,
            )?;
        }
        Ok(changed)
    }
}

pub(super) fn stored_routine_dependents(
    registry: &BTreeMap<String, Vec<Arc<SQLUserFunction>>>,
    target: &RoutineDropTarget,
) -> Result<Vec<RoutineDropTarget>, SQLError> {
    let binding = target.binding();
    let mut dependents = Vec::new();
    for (name, overloads) in registry {
        for function in overloads {
            if routine_definition_references(&function.def, &binding)? {
                dependents.push(RoutineDropTarget {
                    object_id: function.def.object_id,
                    name: name.clone(),
                    argument_types: routine_signature_types(&function.def),
                    is_procedure: function.def.is_procedure,
                });
            }
        }
    }
    dependents.retain(|dependent| dependent != target);
    dependents.sort();
    dependents.dedup();
    Ok(dependents)
}

fn routine_definition_references(
    def: &CreateFunction,
    target: &uqa_sql::ast::FunctionBinding,
) -> Result<bool, SQLError> {
    for default in def
        .params
        .iter()
        .filter_map(|parameter| parameter.default.as_ref())
    {
        if crate::engine_events::expression_references_routine_identity(default, target)? {
            return Ok(true);
        }
    }
    let FunctionBody::Statements(statements) = &def.body else {
        return Ok(false);
    };
    for statement in statements {
        if crate::engine_events::statement_references_routine_identity(statement, target)? {
            return Ok(true);
        }
    }
    Ok(false)
}
