//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! First-reached preparation of one PL/pgSQL embedded SQL occurrence.

use super::Interpreter;
use parking_lot::Mutex;
use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Weak,
    },
};
use uqa_sql::{
    ast::{Expr, Statement},
    binding::statements::ProceduralPlanAnalysis,
    plan::UnifiedPlan,
    plpgsql::{
        parameterize_statement_variables, PLpgSQLCursorArguments, PLpgSQLExpression,
        PLpgSQLStatement, PLpgSQLVariableReference,
    },
    prepared::{definition::analysis_is_current, dependencies::PreparedAnalysisDependencies},
    SQLError, SQLParam,
};

/// Owned by one compiled body/specialization. DO blocks hold it only for their activation.
#[derive(Default)]
pub struct PLpgSQLPreparations {
    fragments: Mutex<BTreeMap<usize, Arc<PreparedFragment>>>,
}

pub(super) struct PreparedFragment {
    pub syntax: Statement,
    pub plan: UnifiedPlan,
    pub analysis: ProceduralPlanAnalysis,
    valid: AtomicBool,
    pub variables: Vec<PLpgSQLVariableReference>,
}
/// Weak owners include anonymous blocks and old routine activations that remain
/// live after replacement. Their prepared inputs receive the same publication events.
#[derive(Default)]
pub(crate) struct PLpgSQLPreparationRegistry {
    owners: Mutex<Vec<Weak<PLpgSQLPreparations>>>,
}
impl PLpgSQLPreparationRegistry {
    pub(crate) fn register(&self) -> Arc<PLpgSQLPreparations> {
        let owner = Arc::new(PLpgSQLPreparations::default());
        let mut owners = self.owners.lock();
        owners.retain(|owner| owner.strong_count() != 0);
        owners.push(Arc::downgrade(&owner));
        owner
    }
    pub(crate) fn invalidate(&self, affects: &impl Fn(&PreparedAnalysisDependencies) -> bool) {
        self.owners.lock().retain(|owner| {
            let Some(owner) = owner.upgrade() else {
                return false;
            };
            owner.invalidate(affects);
            true
        });
    }
}
impl PLpgSQLPreparations {
    fn invalidate(&self, affects: &impl Fn(&PreparedAnalysisDependencies) -> bool) {
        for fragment in self.fragments.lock().values() {
            if affects(&fragment.analysis.dependencies) {
                fragment.valid.store(false, Ordering::Release);
            }
        }
    }
    fn get_or_prepare(
        &self,
        site: usize,
        current: impl FnOnce(&PreparedFragment) -> Result<bool, SQLError>,
        prepare: impl FnOnce(Option<&Statement>) -> Result<PreparedFragment, SQLError>,
    ) -> Result<Arc<PreparedFragment>, SQLError> {
        let retained = self.fragments.lock().get(&site).cloned();
        if let Some(prepared) = &retained {
            if prepared.valid.load(Ordering::Acquire) && current(prepared)? {
                return Ok(Arc::clone(prepared));
            }
            // Reanalysis failures keep the successfully parsed syntax, but must
            // never make the old cooked inputs valid again when the path changes back.
            prepared.valid.store(false, Ordering::Release);
        }
        // Parsing/analysis can invoke other routines or publish catalog changes.
        // Never hold either the fragment or registry lock across these operations.
        let prepared = Arc::new(prepare(retained.as_ref().map(|entry| &entry.syntax))?);
        let mut fragments = self.fragments.lock();
        if let Some(inner) = fragments.get(&site) {
            if !retained.as_ref().is_some_and(|old| Arc::ptr_eq(old, inner)) {
                return Ok(Arc::clone(inner));
            }
        }
        fragments.insert(site, Arc::clone(&prepared));
        Ok(prepared)
    }
}
impl PreparedFragment {
    pub fn direct_routine(&self) -> bool {
        matches!(
            self.syntax,
            Statement::Call { .. } | Statement::DoBlock { .. }
        )
    }
    pub fn is_call(&self) -> bool {
        matches!(self.syntax, Statement::Call { .. })
    }
    pub fn expression(&self) -> Result<&Expr, SQLError> {
        let Statement::Select(query) = &self.syntax else {
            return Err(SQLError::Internal(
                "PL/pgSQL expression has no SELECT".into(),
            ));
        };
        if query.projections.len() != 1 {
            return Err(SQLError::Internal(
                "PL/pgSQL expression has multiple targets".into(),
            ));
        }
        Ok(&query.projections[0].expr)
    }
}
impl Interpreter<'_> {
    pub(super) fn prepare_expression(
        &self,
        expression: &PLpgSQLExpression,
    ) -> Result<Arc<PreparedFragment>, SQLError> {
        self.prepare_fragment(expression.site(), || expression.parse_statement())
    }
    pub(super) fn prepare_statement(
        &self,
        statement: &PLpgSQLStatement,
    ) -> Result<Arc<PreparedFragment>, SQLError> {
        self.prepare_fragment(statement.site(), || statement.parse())
    }
    pub(super) fn prepare_cursor_arguments(
        &self,
        arguments: &PLpgSQLCursorArguments,
    ) -> Result<Arc<PreparedFragment>, SQLError> {
        self.prepare_fragment(arguments.site(), || arguments.parse_statement())
    }
    fn prepare_fragment(
        &self,
        site: usize,
        parse: impl FnOnce() -> Result<Statement, SQLError>,
    ) -> Result<Arc<PreparedFragment>, SQLError> {
        self.services.runtime.cancellation_token().check()?;
        self.preparations.get_or_prepare(
            site,
            |prepared| {
                let Some(context) = self.services.statements.body_input_context() else {
                    return Ok(true);
                };
                analysis_is_current(
                    &context.analysis,
                    prepared.analysis.effective_search_path.as_ref(),
                    &prepared.analysis.dependencies,
                    prepared.analysis.dependency_snapshot.as_ref(),
                )
            },
            |retained| {
                let syntax = if let Some(syntax) = retained {
                    syntax.clone()
                } else {
                    let (syntax, metadata) = uqa_sql::parser::with_settings(
                        self.services.statements.parser_settings(),
                        parse,
                    );
                    for notice in metadata.notices.iter() {
                        self.services.runtime.push_notice(notice.clone());
                    }
                    syntax?
                };
                let bound = parameterize_statement_variables(
                    &syntax,
                    &mut self.resolver(),
                    self.variable_conflict,
                    &mut |statement, parameters, names| {
                        self.resolve_variable_sites(statement, &parameters, names)
                    },
                )?;
                let mut plan = UnifiedPlan::lower_with(bound.statement, &|name: &str| {
                    self.services.runtime.has_aggregate_function(name)
                });
                let analysis = self
                    .services
                    .statements
                    .analyze_static_plan(&mut plan, &bound.parameters)?;
                // Only a completely analyzed statement is retained. Parse and input
                // failures retry; runtime errors cannot undo an already prepared site.
                Ok(PreparedFragment {
                    syntax,
                    plan,
                    analysis,
                    valid: AtomicBool::new(true),
                    variables: bound.references,
                })
            },
        )
    }
    pub(super) fn fragment_parameters(
        &self,
        fragment: &PreparedFragment,
    ) -> Result<Vec<SQLParam>, SQLError> {
        let mut resolver = self.resolver();
        fragment
            .variables
            .iter()
            .map(|reference| reference.read(&mut resolver))
            .collect()
    }
    pub(super) fn execute_fragment(
        &self,
        prepared: &PreparedFragment,
    ) -> Result<uqa_sql::SQLResult, SQLError> {
        let parameters = self.fragment_parameters(prepared)?;
        let _guard = prepared
            .direct_routine()
            .then(|| super::DirectRoutineCommandGuard::enter(self.services.session));
        self.services
            .statements
            .execute_body_statement(prepared.plan.clone(), &parameters, None)
    }
}

#[cfg(test)]
mod tests;
