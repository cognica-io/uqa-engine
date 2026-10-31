//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind SQL-body parameters and retain analysis at the statement's first use.

use super::inputs::{SQLBodyIdentity, SQLRoutineInputContext};
use crate::routines::{context::StatementResultCheck, RoutineContext};
use uqa_sql::{
    binding::{
        bind_routine_parameter_references, statements::AnalyzedResult, RoutineParameterScope,
    },
    plan::{CommandPlan, UnifiedPlan},
    routines::{
        body_validation::{reject_output_argument_call, reject_undefined_parameters},
        declaration::RoutineTypeCatalog,
        resolution::RoutineOverloadContext,
    },
    ColumnType, SQLError, SQLParam,
};

pub(super) struct SQLStatements<'a> {
    pub control: uqa_core::memory::ProductionControl<'a>,
    pub context: RoutineContext<'a>,
    pub types: &'a dyn RoutineTypeCatalog,
    pub overloads: &'a RoutineOverloadContext<'a>,
    pub params: &'a [SQLParam],
    pub parameters: Option<RoutineParameterScope>,
    pub identity: SQLBodyIdentity,
    pub parameter_types: &'a [ColumnType],
    pub inputs: Option<SQLRoutineInputContext<'a>>,
}

pub(super) enum SQLBodyPlan {
    Selected(uqa_sql::plan::SharedExecutablePlan),
    Uncached(UnifiedPlan),
}

impl SQLBodyPlan {
    pub(super) fn plan(&self) -> &UnifiedPlan {
        match self {
            Self::Selected(plan) => plan,
            Self::Uncached(plan) => plan,
        }
    }

    pub(super) fn execute(
        self,
        context: RoutineContext<'_>,
        params: &[SQLParam],
        check: Option<StatementResultCheck<'_>>,
    ) -> Result<uqa_sql::SQLResult, SQLError> {
        match self {
            Self::Selected(plan) => context.statements.execute_selected_body_plan(&plan, params),
            Self::Uncached(plan) => context
                .statements
                .execute_body_statement(plan, params, check),
        }
    }
}

impl SQLStatements<'_> {
    pub(super) fn prepare(
        &self,
        plan: &UnifiedPlan,
        position: usize,
        check: Option<StatementResultCheck<'_>>,
    ) -> Result<SQLBodyPlan, SQLError> {
        let Some(inputs) = &self.inputs else {
            return self.bind(plan).map(SQLBodyPlan::Uncached);
        };
        let analyzed = inputs.cache.statement_with_control(
            self.identity,
            position,
            self.parameter_types,
            &inputs.analysis,
            &self.control,
            || {
                uqa_sql::prepared::definition::analyze_routine_definition_owned(
                    &inputs.analysis,
                    self.types,
                    self.bind_owned(plan)?,
                    self.parameter_types,
                    &|schema| {
                        if let Some(check) = check {
                            check(
                                &schema
                                    .cloned()
                                    .map_or(AnalyzedResult::Command, AnalyzedResult::Schema),
                            )?;
                        }
                        Ok(())
                    },
                    &self.control,
                )?
                .into_shared_with_control(&self.control)
            },
        )?;
        if let Some(check) = check {
            check(
                &analyzed
                    .definition
                    .result_schema
                    .clone()
                    .map_or(AnalyzedResult::Command, AnalyzedResult::Schema),
            )?;
        }
        analyzed
            .select(|entry| {
                self.context
                    .statements
                    .select_body_plan(entry, self.params, &self.control)
            })
            .map(SQLBodyPlan::Selected)
    }

    fn bind_owned(
        &self,
        plan: &UnifiedPlan,
    ) -> Result<uqa_sql::plan::MutableExecutablePlan, SQLError> {
        let mut statement = Some(plan.copy_for_rewrite(&self.control)?);
        if let Some(parameters) = &self.parameters {
            self.context.statements.with_statement_scope_with_control(
                &self.control,
                &mut |routines, binding| {
                    let plan = statement.take().ok_or_else(|| {
                        SQLError::Internal("routine binding scope ran twice".into())
                    })?;
                    statement = Some(uqa_sql::binding::bind_routine_parameter_references_owned(
                        routines,
                        plan,
                        self.params,
                        binding,
                        parameters,
                        &self.control,
                    )?);
                    Ok(())
                },
            )?;
        }
        let statement = statement.expect("routine parameter binding preserves its statement");
        uqa_sql::routines::body_validation::reject_undefined_parameters_with_control(
            statement.as_plan_ref(),
            self.params.len(),
            &self.control,
        )?;
        if let uqa_sql::plan::PlanRef::Command(CommandPlan::Call { name, args }) =
            statement.as_plan_ref()
        {
            uqa_sql::routines::body_validation::reject_output_argument_call_with_control(
                self.overloads,
                self.types,
                name,
                args,
                &mut |argument| {
                    self.context.expressions.expression_type_with_control(
                        argument,
                        self.params,
                        &self.control,
                    )
                },
                &self.control,
            )?;
        }
        Ok(statement)
    }

    fn bind(&self, plan: &UnifiedPlan) -> Result<UnifiedPlan, SQLError> {
        let mut statement = plan.clone();
        if let Some(parameters) = &self.parameters {
            self.context
                .statements
                .with_statement_scope(&mut |routines, ctes| {
                    bind_routine_parameter_references(
                        routines,
                        &mut statement,
                        self.params,
                        ctes,
                        parameters,
                    )
                })?;
        }
        reject_undefined_parameters(&mut statement, self.params.len())?;
        if let UnifiedPlan::Command(command) = &statement {
            if let CommandPlan::Call { name, args } = command.as_ref() {
                reject_output_argument_call(
                    self.overloads,
                    self.types,
                    name,
                    args,
                    &mut |argument| {
                        self.context
                            .expressions
                            .expression_type(argument, self.params)
                    },
                )?;
            }
        }
        Ok(statement)
    }
}
