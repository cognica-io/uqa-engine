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

use super::{CteScope, RowSchema, SchemaScope};
use crate::engine_user_functions::RoutineResolution;
use parameters::{error, ExpressionType, ParameterTypes};
use uqa_execution::ScalarExpr;
use uqa_planner::{QueryPlan, UnifiedPlan};
use uqa_sql::{ColumnType, SQLError};

pub(in crate::sql) fn infer_prepared_parameter_types(
    routines: &dyn RoutineResolution,
    plan: &UnifiedPlan,
    declared: &[Option<ColumnType>],
    ctes: &CteScope,
) -> Result<Vec<Option<ColumnType>>, SQLError> {
    let mut analysis = Preparation {
        routines,
        scope: SchemaScope::for_analysis(ctes)?,
        parameters: ParameterTypes::new(declared),
    };
    match plan {
        UnifiedPlan::Query(query) => {
            analysis.query(query, None)?;
        }
        UnifiedPlan::Command(command) => {
            analysis.command(command)?;
        }
    }
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
                    ty.sql_name()
                ),
            ));
        }
        if let ScalarExpr::Literal(value @ uqa_core::Value::Str(_)) = expression {
            uqa_sql::expr::cast_value(value, "boolean")?;
        }
        Ok(())
    }
}
