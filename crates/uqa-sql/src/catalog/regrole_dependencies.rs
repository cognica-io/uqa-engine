//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` dependency restrictions for stored `regrole` constants.

use crate::ast::{ColumnType, Expr};
use crate::plan::{QueryPlan, UnifiedPlan};
use crate::SQLError;
use crate::ScalarExpr;
use uqa_core::Value;

use crate::expr::EngineHook;

pub trait StoredRegroleResolver {
    fn resolve_stored_regrole(&self, name: &str) -> Result<Option<i64>, SQLError>;
}
impl<T: EngineHook + ?Sized> StoredRegroleResolver for T {
    fn resolve_stored_regrole(&self, name: &str) -> Result<Option<i64>, SQLError> {
        EngineHook::resolve_regrole(self, name)
    }
}

fn scalar_regrole_type(ty: &ColumnType) -> bool {
    match ty {
        ColumnType::Regrole => true,
        ColumnType::Domain { base, .. } => scalar_regrole_type(base),
        _ => false,
    }
}

fn scalar_regrole_type_name(name: &str) -> bool {
    ColumnType::from_sql_name(name).is_ok_and(|ty| scalar_regrole_type(&ty))
}

fn regrole_constant_error() -> SQLError {
    SQLError::Routine {
        sqlstate: "0A000".into(),
        message: "constant of the type regrole cannot be used here".into(),
    }
}

fn scalar_regrole_literal(expression: &ScalarExpr) -> Option<&str> {
    let ScalarExpr::Cast { expr, ty } = expression else {
        return None;
    };
    if !scalar_regrole_type_name(ty) {
        return None;
    }
    match expr.as_ref() {
        ScalarExpr::Literal(Value::Str(input)) => Some(input),
        _ => None,
    }
}

#[derive(Default)]
pub struct StoredRegroleConstants {
    inputs: Vec<String>,
}

impl StoredRegroleConstants {
    pub fn collect_expression(
        &mut self,
        expression: &Expr,
        assignment_target: Option<&ColumnType>,
    ) {
        if assignment_target.is_some_and(scalar_regrole_type) {
            if let Expr::Literal(Value::Str(input)) = expression {
                self.inputs.push(input.clone());
            }
        }
        let scalar = crate::plan::ExpressionPlan::lower(expression.clone()).scalar;
        scalar.visit(&mut |node| {
            if let Some(input) = scalar_regrole_literal(node) {
                self.inputs.push(input.to_string());
            }
        });
    }

    pub fn collect_query_plan(&mut self, plan: &mut QueryPlan) {
        plan.rewrite_scalar_expressions(&mut |expression| {
            if let Some(input) = scalar_regrole_literal(expression) {
                self.inputs.push(input.to_string());
            }
        });
    }

    pub fn collect_plan(&mut self, plan: &mut UnifiedPlan) {
        plan.rewrite_scalar_expressions(&mut |expression| {
            if let Some(input) = scalar_regrole_literal(expression) {
                self.inputs.push(input.to_string());
            }
        });
    }

    pub fn validate_inputs(&self, context: &dyn EngineHook) -> Result<(), SQLError> {
        self.validate_inputs_with(context)
    }

    pub fn validate_inputs_with<C: StoredRegroleResolver + ?Sized>(
        &self,
        context: &C,
    ) -> Result<(), SQLError> {
        for input in &self.inputs {
            context.resolve_stored_regrole(input)?;
        }
        Ok(())
    }

    pub fn reject(&self, context: &dyn EngineHook) -> Result<(), SQLError> {
        self.reject_with(context)
    }

    pub fn reject_with<C: StoredRegroleResolver + ?Sized>(
        &self,
        context: &C,
    ) -> Result<(), SQLError> {
        self.validate_inputs_with(context)?;
        if self.inputs.is_empty() {
            Ok(())
        } else {
            Err(regrole_constant_error())
        }
    }
}

pub fn reject_stored_regrole_constants(
    context: &dyn EngineHook,
    expression: &Expr,
    assignment_target: Option<&ColumnType>,
) -> Result<(), SQLError> {
    let mut constants = StoredRegroleConstants::default();
    constants.collect_expression(expression, assignment_target);
    constants.reject(context)
}

pub fn reject_stored_query_regrole_constants(
    context: &dyn EngineHook,
    plan: &mut QueryPlan,
) -> Result<(), SQLError> {
    let mut constants = StoredRegroleConstants::default();
    constants.collect_query_plan(plan);
    constants.reject(context)
}

pub fn reject_stored_plan_regrole_constants(
    context: &dyn EngineHook,
    plan: &mut UnifiedPlan,
) -> Result<(), SQLError> {
    let mut constants = StoredRegroleConstants::default();
    constants.collect_plan(plan);
    constants.reject(context)
}
