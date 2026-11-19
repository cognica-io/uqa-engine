//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Captured SQL bodies keep their declaration's parameter scope and statement boundaries.

use crate::{
    ast::{CreateFunction, Expr, FunctionBody, Statement},
    catalog::work::{inline, Work},
    ir::observation::ScalarObserver,
    plan::{CommandPlan, PlanRef, StoredStatementObserver, UnifiedPlan},
    routines::{CompiledFunctionBody, RoutineBody, SQLUserFunction},
    schema::dependencies::{observe_schema_expression, SchemaObserver},
    SQLError, ScalarExpr,
};
use uqa_core::memory::ProductionControl;

/// Validate each independently retained body owner without compiling source text, resolving replacement bindings or executing a plan.
pub fn validate_captured_body(
    routine: &SQLUserFunction,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    inline(validate_captured_body_with_work(routine, control, &mut ()))
}

pub async fn validate_captured_body_with_work(
    routine: &SQLUserFunction,
    control: &ProductionControl<'_>,
    work: &mut (impl Work + Send),
) -> Result<(), SQLError> {
    control.check()?;
    let definition = &routine.def;
    let mut arity = 0;
    for parameter in &definition.params {
        work.checkpoint().await?;
        control.check()?;
        arity += usize::from(CreateFunction::is_identity_param(parameter));
        if let Some(default) = &parameter.default {
            observe_schema_expression(default, control, work, &mut Parameters(0)).await?;
        }
    }
    match (&definition.body, &routine.body) {
        (FunctionBody::Source(_), RoutineBody::Source) => (),
        (FunctionBody::Statements(statements), RoutineBody::Bound(body)) => {
            if definition.language != "sql" {
                return Err(invalid("SQL-standard body language"));
            }
            let CompiledFunctionBody::SQL(plans) = &***body else {
                return Err(invalid("session-compiled body"));
            };
            if statements.len() != plans.len() {
                return Err(invalid("statement count"));
            }
            let mut parameters = Parameters(arity);
            for (statement, plan) in statements.iter().zip(plans) {
                work.checkpoint().await?;
                control.check()?;
                if !statement_kind_matches(statement, plan) {
                    return Err(invalid("statement kind"));
                }
                UnifiedPlan::visit_stored_statement_with_work(
                    statement,
                    control,
                    work,
                    &mut parameters,
                )
                .await?;
                parameters.plan(plan.into(), control, work).await?;
            }
        }
        _ => return Err(invalid("source and bound body form")),
    }
    Ok(control.check()?)
}

fn statement_kind_matches(statement: &Statement, plan: &UnifiedPlan) -> bool {
    match (statement, plan) {
        (Statement::Select(_) | Statement::Values { .. }, UnifiedPlan::Query(_)) => true,
        (statement, UnifiedPlan::Command(command)) => matches!(
            (statement, &**command),
            (Statement::Insert(_), CommandPlan::Insert(_))
                | (Statement::Update(_), CommandPlan::Update(_))
                | (Statement::Delete(_), CommandPlan::Delete(_))
                | (Statement::Merge(_), CommandPlan::Merge(_))
        ),
        _ => false,
    }
}

struct Parameters(usize);

impl ScalarObserver for Parameters {
    fn scalar<W: Work + Send>(
        &mut self,
        value: &ScalarExpr,
        _: &mut W,
    ) -> impl std::future::Future<Output = Result<(), SQLError>> + Send {
        std::future::ready(match value {
            ScalarExpr::Param(index)
            | ScalarExpr::TypedLiteral {
                parameter_index: Some(index),
                ..
            } => parameter_ordinal(*index, self.0),
            _ => Ok(()),
        })
    }
}

impl SchemaObserver for Parameters {
    fn expression<W: Work + Send>(
        &mut self,
        value: &Expr,
        _: &mut W,
    ) -> impl std::future::Future<Output = Result<(), SQLError>> + Send {
        std::future::ready(if let Expr::Param(index) = value {
            parameter_ordinal(*index, self.0)
        } else {
            Ok(())
        })
    }
}

impl StoredStatementObserver for Parameters {
    async fn plan<W: Work + Send>(
        &mut self,
        plan: PlanRef<'_>,
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> Result<(), SQLError> {
        crate::plan::visit_scalar_nodes_with_work(plan, control, work, self).await
    }
}

fn parameter_ordinal(index: usize, arity: usize) -> Result<(), SQLError> {
    if index == 0 || index > arity {
        return Err(invalid("parameter ordinal"));
    }
    Ok(())
}

fn invalid(detail: &str) -> SQLError {
    SQLError::Internal(format!("invalid native routine body: {detail}"))
}

#[cfg(test)]
mod tests;
