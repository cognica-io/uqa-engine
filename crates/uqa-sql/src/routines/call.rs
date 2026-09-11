//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Procedure argument validation, static overload selection, and declared result schemas.

use crate::{
    assignment::conversion::column_type_name,
    ast::ColumnType,
    ir::{analyze_expression_call_arguments, ScalarCallArgument},
    plan::ExpressionPlan,
    routines::{
        declaration::RoutineTypeCatalog,
        resolution::{RoutineCallKind, RoutineOverloadContext},
    },
    RowSchema, SQLError, ScalarExpr,
};
use uqa_core::Value;

/// Reject forbidden subqueries before the caller captures its statement catalog scope.
pub fn validate_call_arguments(arguments: &[ExpressionPlan]) -> Result<(), SQLError> {
    if arguments
        .iter()
        .any(|argument| !argument.subqueries.is_empty())
    {
        return Err(SQLError::Unsupported(
            "cannot use subquery in CALL argument".into(),
        ));
    }
    Ok(())
}

/// Unknown string and NULL literals participate in procedure overload resolution without a concrete type.
pub fn infer_call_argument_types(
    arguments: &[ExpressionPlan],
    decoded: &[ScalarCallArgument<'_>],
    infer: &mut dyn FnMut(&ExpressionPlan) -> Result<Option<ColumnType>, SQLError>,
) -> Result<Vec<Option<ColumnType>>, SQLError> {
    arguments
        .iter()
        .zip(decoded)
        .map(|(argument, call_argument)| {
            if matches!(
                call_argument.value,
                ScalarExpr::Literal(Value::Str(_) | Value::Null)
            ) {
                Ok(None)
            } else {
                infer(argument)
            }
        })
        .collect()
}

/// The syntax metadata needed before the caller captures the scope used for result description.
pub struct ProcedureCallAnalysis<'a> {
    arguments: &'a [ExpressionPlan],
    decoded: Vec<ScalarCallArgument<'a>>,
    names: Vec<Option<String>>,
    explicit_variadic: bool,
}

impl<'a> ProcedureCallAnalysis<'a> {
    pub fn new(arguments: &'a [ExpressionPlan]) -> Result<Self, SQLError> {
        validate_call_arguments(arguments)?;
        let (decoded, explicit_variadic) = analyze_expression_call_arguments(arguments)?;
        let names = decoded
            .iter()
            .map(|argument| argument.name.map(str::to_string))
            .collect();
        Ok(Self {
            arguments,
            decoded,
            names,
            explicit_variadic,
        })
    }

    pub fn result_schema(
        &self,
        name: &str,
        overloads: &RoutineOverloadContext<'_>,
        types: &dyn RoutineTypeCatalog,
        infer: &mut dyn FnMut(&ExpressionPlan) -> Result<Option<ColumnType>, SQLError>,
    ) -> Result<Option<RowSchema>, SQLError> {
        let argument_types = infer_call_argument_types(self.arguments, &self.decoded, infer)?;
        let Some(resolved) = overloads.resolve_static_sql_routine_match(
            name,
            None,
            &self.names,
            &argument_types,
            self.explicit_variadic,
            RoutineCallKind::Procedure,
        )?
        else {
            let signature = argument_types
                .iter()
                .map(|argument| {
                    argument
                        .as_ref()
                        .map_or_else(|| "unknown", column_type_name)
                })
                .collect::<Vec<_>>()
                .join(", ");
            return Err(SQLError::Routine {
                sqlstate: "42883".into(),
                message: format!("procedure {name}({signature}) does not exist"),
            });
        };
        super::invocation::call_output_schema(
            types,
            &resolved.function.def,
            &resolved.invocation.parameter_types,
        )
    }
}
