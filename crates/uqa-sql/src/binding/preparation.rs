//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Preparation-time semantic analysis with ordered parameter coercion.

mod commands;
mod ctes;
mod expression_contexts;
mod expressions;
mod parameters;
mod queries;
mod sources;
#[cfg(test)]
mod tests;

use super::{BindingContext, RowSchema, SchemaScope};
use crate::plan::{QueryPlan, UnifiedPlan};
use crate::routines::RoutineResolution;
use crate::ScalarExpr;
use crate::{ColumnType, SQLError};
use parameters::{error, ExpressionType, ParameterTypes};

/// Read input constants while the prepared definition's original tree stays in place. The short-lived literal identities never escape this operation; only converted values enter the stored plan.
pub(crate) fn read_prepared_inputs(
    routines: &dyn RoutineResolution,
    plan: &mut UnifiedPlan,
    declared: &[Option<ColumnType>],
    ctes: &BindingContext,
) -> Result<Vec<Option<ColumnType>>, SQLError> {
    let mut analysis = Preparation {
        routines,
        scope: SchemaScope::for_analysis(ctes)?,
        parameters: ParameterTypes::with_input_constants(declared),
    };
    analysis.plan(plan)?;
    let constants = analysis.parameters.take_input_constants();
    let parameters = analysis.parameters.finish()?;
    constants.apply(plan)?;
    Ok(parameters)
}

pub fn infer_prepared_parameter_types(
    routines: &dyn RoutineResolution,
    plan: &UnifiedPlan,
    declared: &[Option<ColumnType>],
    ctes: &BindingContext,
) -> Result<Vec<Option<ColumnType>>, SQLError> {
    let mut analysis = Preparation {
        routines,
        scope: SchemaScope::for_analysis(ctes)?,
        parameters: ParameterTypes::new(declared),
    };
    analysis.plan(plan)?;
    analysis.parameters.finish()
}

struct Preparation<'a> {
    routines: &'a dyn RoutineResolution,
    scope: SchemaScope,
    parameters: ParameterTypes,
}

struct QueryOutput {
    columns: Vec<String>,
    types: Vec<ExpressionType>,
    open: bool,
}

impl QueryOutput {
    fn schema(&self) -> RowSchema {
        let schema = RowSchema::with_types(
            self.columns.clone(),
            self.types.iter().map(|value| value.ty.clone()).collect(),
        );
        if self.open {
            RowSchema::with_open_columns(&schema, None)
        } else {
            schema
        }
    }
}

impl Preparation<'_> {
    fn plan(&mut self, plan: &UnifiedPlan) -> Result<(), SQLError> {
        match plan {
            UnifiedPlan::Query(query) => {
                self.query(query, None)?;
            }
            UnifiedPlan::Command(command) => {
                self.command(command)?;
            }
        }
        Ok(())
    }

    fn known_type(
        &mut self,
        expression: &ScalarExpr,
        input: &RowSchema,
        subqueries: &[QueryPlan],
    ) -> Result<Option<ColumnType>, SQLError> {
        self.scope.bind_expression_type(
            self.routines,
            expression,
            input,
            subqueries,
            &self.parameters.values(),
            Some(input),
        )
    }

    fn type_name(&self, name: &str) -> Result<ColumnType, SQLError> {
        self.routines
            .resolve_type_name(name)?
            .map_or_else(|| ColumnType::from_sql_name(name), Ok)
    }

    fn require_boolean(
        &mut self,
        expression: &ScalarExpr,
        input: &RowSchema,
        subqueries: &[QueryPlan],
        context: &str,
    ) -> Result<(), SQLError> {
        let mut observed = self.expression(expression, input, subqueries)?;
        self.parameters
            .coerce_unknown(&mut observed, &ColumnType::Boolean)?;
        let Some(mut ty) = observed.ty.as_ref() else {
            return Ok(());
        };
        while let ColumnType::Domain { base, .. } = ty {
            ty = base;
        }
        if !matches!(ty, ColumnType::Boolean) {
            return Err(error(
                "42804",
                format!(
                    "argument of {context} must be type boolean, not type {}",
                    ty.regtype_name()
                ),
            ));
        }
        if let ScalarExpr::Literal(value @ uqa_core::Value::Str(_)) = expression {
            crate::expr::cast_value(value, "boolean")?;
        }
        Ok(())
    }
}
