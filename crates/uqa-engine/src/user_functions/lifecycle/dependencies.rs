//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Creation-time binding and lifecycle traversal for routine-owned routine dependencies.

use uqa_sql::ast::{CreateFunction, FunctionBody};
use uqa_sql::SQLError;

use crate::capabilities::RelationLookupMode;
use crate::Engine;

use super::super::declaration::compile_function_body;
use super::CompiledFunctionBody;

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
        let body_changed = self.bind_sql_standard_body_routines(def, &compiled)?
            | self.bind_routine_regclass_constants(def)?;
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
            changed |= crate::events::bind_stored_expression_routines(default, &references)?;
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
            changed |= self.bind_stored_merge_target_columns(statement)?;
            changed |= self.bind_stored_statement_source_columns(statement)?;
        }
        Ok(changed)
    }

    fn bind_sql_standard_body_routines(
        &self,
        def: &mut CreateFunction,
        compiled: &CompiledFunctionBody,
    ) -> Result<bool, SQLError> {
        let dependency_body = self.stored_merge_dependency_body(def)?;
        let compiled = dependency_body.as_ref().unwrap_or(compiled);
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
            changed |=
                crate::events::bind_stored_statement_routines(statement, &routines.references)?;
        }
        Ok(changed)
    }
}
