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
    pub context: RoutineContext<'a>,
    pub types: &'a dyn RoutineTypeCatalog,
    pub overloads: &'a RoutineOverloadContext<'a>,
    pub params: &'a [SQLParam],
    pub parameters: Option<RoutineParameterScope>,
    pub identity: SQLBodyIdentity,
    pub parameter_types: Vec<ColumnType>,
    pub inputs: Option<SQLRoutineInputContext<'a>>,
}

impl SQLStatements<'_> {
    pub(super) fn prepare(
        &self,
        plan: &UnifiedPlan,
        position: usize,
        check: Option<StatementResultCheck<'_>>,
    ) -> Result<UnifiedPlan, SQLError> {
        let Some(inputs) = &self.inputs else {
            return self.bind(plan);
        };
        let analyzed = inputs.cache.statement(
            self.identity,
            position,
            &self.parameter_types,
            &inputs.analysis,
            || {
                let mut definition = uqa_sql::prepared::definition::analyze_definition(
                    &inputs.analysis,
                    self.bind(plan)?,
                    &self.parameter_types,
                )?;
                if let Some(check) = check {
                    let result = definition
                        .result_schema
                        .clone()
                        .map_or(AnalyzedResult::Command, AnalyzedResult::Schema);
                    check(&result)?;
                }
                uqa_sql::routines::compilation::bind_analyzed_sql_body_types(
                    self.types,
                    &mut definition.logical_plan,
                )?;
                Ok(definition)
            },
        )?;
        Ok(analyzed.logical_plan.clone())
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
