//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Captured SQL bodies keep their declaration's parameter scope and statement boundaries.

use crate::{
    ast::{CreateFunction, Expr, FunctionBody, Statement},
    plan::{CommandPlan, PlanRef, UnifiedPlan},
    routines::{CompiledFunctionBody, RoutineBody, SQLUserFunction},
    SQLError, ScalarExpr,
};
use uqa_core::memory::ProductionControl;

/// Validate each independently retained body owner without compiling source text, resolving replacement bindings or executing a plan.
pub fn validate_captured_body(
    routine: &SQLUserFunction,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    control.check()?;
    let definition = &routine.def;
    let mut arity = 0;
    for parameter in &definition.params {
        control.check()?;
        arity += usize::from(CreateFunction::is_identity_param(parameter));
        if let Some(default) = &parameter.default {
            crate::schema::dependencies::walk_schema_expr_with_control(
                default,
                control,
                &mut |node| {
                    if let Expr::Param(index) = node {
                        parameter_ordinal(*index, 0)?;
                    }
                    Ok(())
                },
            )?;
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
            for (statement, plan) in statements.iter().zip(plans) {
                control.check()?;
                if !statement_kind_matches(statement, plan) {
                    return Err(invalid("statement kind"));
                }
                let syntax =
                    UnifiedPlan::lower_borrowed_for_rewrite(statement, &|_: &str| false, control)?;
                parameters(syntax.as_plan_ref(), arity, control)?;
                parameters(plan.into(), arity, control)?;
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

fn parameters(
    plan: PlanRef<'_>,
    arity: usize,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    plan.try_visit_scalar_expressions(&mut |root| {
        root.try_visit(&mut |node| {
            control.check()?;
            if let ScalarExpr::Param(index)
            | ScalarExpr::TypedLiteral {
                parameter_index: Some(index),
                ..
            } = node
            {
                parameter_ordinal(*index, arity)?;
            }
            Ok(true)
        })
    })
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
