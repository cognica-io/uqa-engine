//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! First-reached preparation of one PL/pgSQL embedded SQL occurrence.

use super::Interpreter;
use parking_lot::Mutex;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Weak,
};
use uqa_sql::{
    ast::{Expr, Statement},
    binding::statements::AnalyzedResult,
    plpgsql::{
        parameterize_statement_variables_owned, PLpgSQLCursorArguments, PLpgSQLExpression,
        PLpgSQLSource, PLpgSQLStatement, PLpgSQLVariableReference,
    },
    prepared::{
        definition::analysis_is_current_with_control, dependencies::PreparedAnalysisDependencies,
    },
    SQLError, SQLParam,
};

use uqa_core::memory::{OwnedMap, Produced, ProductionControl, ProductionVec};
use uqa_sql::prepared::ownership::SharedPreparedInput;

type SharedFragment = Arc<Produced<PreparedFragment>>;

/// Owned by one compiled body/specialization. DO blocks hold it only for their activation.
#[derive(Default)]
pub struct PLpgSQLPreparations {
    fragments: Mutex<OwnedMap<usize, SharedFragment>>,
    allocation: Option<Arc<uqa_core::memory::MemoryReservation>>,
}

pub(super) struct PreparedFragment {
    source: Arc<PLpgSQLSource>,
    simple_expression: bool,
    pub syntax: Arc<Statement>,
    pub analysis: FragmentAnalysis,
    valid: AtomicBool,
    pub variables: Produced<Vec<PLpgSQLVariableReference>>,
    parameter_types:
        uqa_sql::prepared::ownership::SharedPreparedInput<Vec<Option<uqa_sql::ColumnType>>>,
    variants: super::plans::RoutinePlanVariants,
}
pub(super) struct FragmentAnalysis {
    pub result: AnalyzedResult,
    pub dependencies: SharedPreparedInput<PreparedAnalysisDependencies>,
    pub effective_search_path:
        SharedPreparedInput<Option<uqa_sql::catalog::resolution::EffectiveSearchPath>>,
    pub dependency_snapshot:
        SharedPreparedInput<Option<uqa_sql::prepared::dependencies::PreparedDependencySnapshot>>,
}
impl FragmentAnalysis {
    fn from_definition(
        definition: &uqa_sql::prepared::definition::PreparedDefinition<
            uqa_sql::plan::SharedExecutablePlan,
        >,
    ) -> Self {
        Self {
            result: definition
                .result_schema
                .clone()
                .map_or(AnalyzedResult::Command, AnalyzedResult::Schema),
            dependencies: definition.dependencies.clone(),
            effective_search_path: definition.effective_search_path.clone(),
            dependency_snapshot: definition.dependency_snapshot.clone(),
        }
    }
}
/// Weak owners include anonymous blocks and old routine activations that remain
/// live after replacement. Their prepared inputs receive the same publication events.
#[derive(Default)]
pub(crate) struct PLpgSQLPreparationRegistry {
    owners: Mutex<OwnedMap<usize, PreparationObserver>>,
}
struct PreparationObserver {
    owner: Weak<PLpgSQLPreparations>,
    // Weak references retain the Arc allocation after its payload is destroyed.
    // Either the live owner or its registry observer keeps this shared lease.
    _allocation: Option<Arc<uqa_core::memory::MemoryReservation>>,
}
impl PLpgSQLPreparationRegistry {
    pub(crate) fn invalidate_execution_plans(&self) {
        self.owners.lock().retain(|_, observer| {
            let Some(owner) = observer.owner.upgrade() else {
                return false;
            };
            for fragment in owner.fragments.lock().values() {
                fragment.variants.invalidate();
            }
            true
        });
    }
    pub(crate) fn register(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<Arc<PLpgSQLPreparations>, SQLError> {
        control.check()?;
        let mut owners = self.owners.lock();
        owners.retain(|_, observer| observer.owner.strong_count() != 0);
        let allocation = control
            .reserve(
                size_of::<PLpgSQLPreparations>() + size_of::<uqa_core::memory::MemoryReservation>(),
            )?
            .map(Arc::new);
        let owner = Arc::new(PLpgSQLPreparations {
            fragments: Mutex::new(OwnedMap::new()),
            allocation,
        });
        let observer = PreparationObserver {
            owner: Arc::downgrade(&owner),
            _allocation: owner.allocation.clone(),
        };
        let key = Arc::as_ptr(&owner) as usize;
        if let Some(budget) = control.budget() {
            owners.insert_budgeted(key, observer, budget)?;
        } else {
            owners.insert(key, observer);
        }
        Ok(owner)
    }
    pub(crate) fn invalidate(&self, affects: &impl Fn(&PreparedAnalysisDependencies) -> bool) {
        self.owners.lock().retain(|_, observer| {
            let Some(owner) = observer.owner.upgrade() else {
                return false;
            };
            owner.invalidate(affects);
            true
        });
    }
}
impl PLpgSQLPreparations {
    #[cfg(test)]
    fn get_or_prepare(
        &self,
        site: usize,
        current: impl FnOnce(&PreparedFragment) -> Result<bool, SQLError>,
        prepare: impl FnOnce(Option<&Arc<Statement>>) -> Result<PreparedFragment, SQLError>,
    ) -> Result<SharedFragment, SQLError> {
        self.get_or_prepare_with_control(site, &ProductionControl::uncontrolled(), current, prepare)
    }
    fn invalidate(&self, affects: &impl Fn(&PreparedAnalysisDependencies) -> bool) {
        for fragment in self.fragments.lock().values() {
            if affects(&fragment.analysis.dependencies) {
                fragment.valid.store(false, Ordering::Release);
            }
        }
    }
    fn get_or_prepare_with_control(
        &self,
        site: usize,
        control: &ProductionControl<'_>,
        current: impl FnOnce(&PreparedFragment) -> Result<bool, SQLError>,
        prepare: impl FnOnce(Option<&Arc<Statement>>) -> Result<PreparedFragment, SQLError>,
    ) -> Result<SharedFragment, SQLError> {
        control.check()?;
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
        let prepared = control
            .finish(
                prepare(retained.as_ref().map(|entry| &entry.syntax))?,
                control.empty_reservation(),
            )?
            .into_shared(control)?;
        let mut fragments = self.fragments.lock();
        if let Some(inner) = fragments.get(&site) {
            if !retained.as_ref().is_some_and(|old| Arc::ptr_eq(old, inner)) {
                return Ok(Arc::clone(inner));
            }
        }
        if let Some(budget) = control.budget() {
            fragments.insert_budgeted(site, Arc::clone(&prepared), budget)?;
        } else {
            fragments.insert(site, Arc::clone(&prepared));
        }
        Ok(prepared)
    }
}
impl PreparedFragment {
    pub fn direct_routine(&self) -> bool {
        matches!(
            self.syntax.as_ref(),
            Statement::Call { .. } | Statement::DoBlock { .. }
        )
    }
    pub fn is_call(&self) -> bool {
        matches!(self.syntax.as_ref(), Statement::Call { .. })
    }
    pub fn expression(&self) -> Result<&Expr, SQLError> {
        let Statement::Select(query) = self.syntax.as_ref() else {
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
    ) -> Result<SharedFragment, SQLError> {
        self.prepare_fragment(expression.site(), expression.source(), || {
            expression.parse_statement()
        })
    }
    pub(super) fn prepare_statement(
        &self,
        statement: &PLpgSQLStatement,
    ) -> Result<SharedFragment, SQLError> {
        self.prepare_fragment(statement.site(), statement.source(), || statement.parse())
    }
    pub(super) fn prepare_cursor_arguments(
        &self,
        arguments: &PLpgSQLCursorArguments,
    ) -> Result<SharedFragment, SQLError> {
        self.prepare_fragment(arguments.site(), arguments.source(), || {
            arguments.parse_statement()
        })
    }
    fn prepare_fragment(
        &self,
        site: usize,
        source: &Arc<PLpgSQLSource>,
        parse: impl FnOnce() -> Result<Statement, SQLError>,
    ) -> Result<SharedFragment, SQLError> {
        self.planning.control().check()?;
        self.services.runtime.cancellation_token().check()?;
        self.preparations
            .get_or_prepare_with_control(
                site,
                &self.planning.control(),
                |prepared| {
                    let Some(context) = self.services.statements.body_input_context() else {
                        return Ok(true);
                    };
                    analysis_is_current_with_control(
                        &context.analysis,
                        prepared.analysis.effective_search_path.as_ref(),
                        &prepared.analysis.dependencies,
                        prepared.analysis.dependency_snapshot.as_ref(),
                        &self.planning.control(),
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
                        Arc::new(syntax?)
                    };
                    let bound = parameterize_statement_variables_owned(
                        &syntax,
                        &mut self.resolver(),
                        self.variable_conflict,
                        &mut |plan, parameters, names, control| {
                            let mut names = Some(names);
                            let mut resolutions = None;
                            self.services.statements.with_statement_scope_with_control(
                                control,
                                &mut |routines, binding| {
                                    resolutions =
                                        Some(uqa_sql::binding::resolve_variable_sites_owned(
                                            routines,
                                            plan,
                                            parameters,
                                            binding,
                                            names.take().ok_or_else(|| {
                                                SQLError::Internal(
                                                    "procedural site scope invoked twice".into(),
                                                )
                                            })?,
                                            control,
                                        )?);
                                    Ok(())
                                },
                            )?;
                            resolutions.ok_or_else(|| {
                                SQLError::Internal("procedural site scope did not run".into())
                            })
                        },
                        &|name: &str| self.services.runtime.has_aggregate_function(name),
                        &self.planning.control(),
                    )?;
                    let analyzed = self.services.statements.analyze_static_plan_owned(
                        bound.plan,
                        &bound.parameters,
                        &self.planning.control(),
                    )?;
                    let definition = analyzed
                        .prepared_definition_owned(&self.planning.control())?
                        .into_shared_with_control(&self.planning.control())?;
                    let analysis = FragmentAnalysis::from_definition(&definition);
                    let variants = super::plans::RoutinePlanVariants::new(&definition);
                    let simple_expression =
                        self.is_simple_fragment(source, &definition.logical_plan)?;
                    // Only a completely analyzed statement is retained. Parse and input
                    // failures retry; runtime errors cannot undo an already prepared site.
                    Ok(PreparedFragment {
                        source: Arc::clone(source),
                        simple_expression,
                        syntax,
                        analysis,
                        valid: AtomicBool::new(true),
                        variables: bound.references,
                        parameter_types: definition.parameter_types,
                        variants,
                    })
                },
            )
            .map_err(|error| source.error_context(error))
    }
    fn is_simple_fragment(
        &self,
        source: &PLpgSQLSource,
        plan: &uqa_sql::plan::UnifiedPlan,
    ) -> Result<bool, SQLError> {
        let mut simple_expression = false;
        self.services.statements.with_statement_scope_with_control(
            &self.planning.control(),
            &mut |routines, _| {
                simple_expression = source.mode != uqa_sql::plpgsql::PLpgSQLParseMode::Statement
                    && uqa_sql::plpgsql::is_simple_expression_plan_with_control(
                        routines,
                        plan,
                        &self.planning.control(),
                    )?;
                Ok(())
            },
        )?;
        Ok(simple_expression)
    }
    pub(super) fn fragment_parameters(
        &self,
        fragment: &PreparedFragment,
    ) -> Result<Produced<Vec<SQLParam>>, SQLError> {
        let mut resolver = self.resolver();
        let control = self.planning.control();
        let mut parameters = ProductionVec::new(control);
        parameters.reserve(fragment.variables.len())?;
        for (reference, expected) in fragment
            .variables
            .iter()
            .zip(fragment.parameter_types.iter())
        {
            let parameter = match reference.read_with_control(&mut resolver, &control) {
                Err(error)
                    if expected.is_some()
                        && matches!(error.sqlstate(), Some("42703" | "55000")) =>
                {
                    parameters.push_produced(SQLParam::deferred_error_with_control(
                        expected.as_ref().expect("retained parameter type"),
                        uqa_sql::ast::DeferredSQLError::capture_with_control(&error, &control)?,
                        &control,
                    )?)?;
                    continue;
                }
                result => result?,
            };
            if let (Some(expected), Some(actual)) = (expected, parameter.declared_scalar_type()) {
                if uqa_sql::catalog::type_metadata::pg_type_oid(expected)
                    != uqa_sql::catalog::type_metadata::pg_type_oid(actual)
                {
                    let number = resolver.parameter_number(reference).ok_or_else(|| {
                        SQLError::Internal(
                            "prepared PL/pgSQL parameter has no datum identity".into(),
                        )
                    })?;
                    parameters.push_produced(SQLParam::deferred_error_with_control(
                        expected,
                        uqa_sql::plpgsql::parameter_type_mismatch_with_control(
                            number, actual, expected, &control,
                        )?,
                        &control,
                    )?)?;
                    continue;
                }
            }
            parameters.push_produced(parameter)?;
        }
        Ok(parameters.finish()?)
    }
    pub(super) fn execute_fragment(
        &self,
        prepared: &PreparedFragment,
    ) -> Result<uqa_sql::SQLResult, SQLError> {
        let parameters = self.fragment_parameters(prepared)?;
        let _guard = prepared
            .direct_routine()
            .then(|| super::DirectRoutineCommandGuard::enter(self.services.session));
        let plan = self.select_fragment_plan(prepared, &parameters)?;
        self.services
            .statements
            .execute_selected_body_plan(&plan, &parameters)
            .map_err(|error| {
                if prepared.simple_expression {
                    error
                } else {
                    prepared.source.error_context(error)
                }
            })
    }

    pub(super) fn select_fragment_plan(
        &self,
        prepared: &PreparedFragment,
        parameters: &[SQLParam],
    ) -> Result<uqa_sql::plan::SharedExecutablePlan, SQLError> {
        prepared
            .variants
            .select(|entry| {
                if prepared.simple_expression {
                    self.services.statements.select_simple_expression_plan(
                        entry,
                        parameters,
                        &self.planning.control(),
                    )
                } else {
                    self.services.statements.select_body_plan(
                        entry,
                        parameters,
                        &self.planning.control(),
                    )
                }
            })
            .map_err(|error| prepared.source.error_context(error))
    }
}

#[cfg(test)]
mod tests;
