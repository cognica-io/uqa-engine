//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The body checks of `PostgreSQL`'s SQL-language validator, `fmgr_sql_validator`, which also hold when a body runs: each statement is analyzed against the catalog before it runs, a reference to a parameter the routine lacks is an error, a `CALL` of a procedure with output arguments is rejected, and the final statement is checked against the declared result.

use super::{
    body_parameters::{sql_body_parameter_scope, sql_body_parameters},
    call::ProcedureCallAnalysis,
    compilation::RoutineCompilationContext,
    declaration::RoutineTypeCatalog,
    resolution::RoutineOverloadContext,
    result_check::check_sql_function_result,
    CompiledFunctionBody,
};
use crate::{
    ast::{ColumnType, CreateFunction, FunctionBody},
    binding::{
        bind_routine_parameter_references,
        statements::{analyze_plan_result, AnalyzedResult, StatementBindingScope},
    },
    plan::{CommandPlan, ExpressionPlan, UnifiedPlan},
    type_resolution::routine_polymorphic_type,
    SQLError, SQLParam, ScalarExpr,
};
use uqa_core::Value;

/// The catalog `CREATE FUNCTION` validates a SQL body against.
pub struct SQLBodyValidationContext<'a> {
    pub compilation: RoutineCompilationContext<'a>,
    pub overloads: RoutineOverloadContext<'a>,
}

/// Whether a SQL body can be analyzed without the argument types of a call: `PostgreSQL` only parses the body of a routine with polymorphic arguments until a call supplies them.
#[must_use]
pub fn sql_body_is_analyzable(def: &CreateFunction) -> bool {
    !def.identity_params()
        .iter()
        .any(|parameter| routine_polymorphic_type(&parameter.type_name).is_some())
}

/// Validate a compiled SQL body as `CREATE FUNCTION` does: analyze each statement in order against the catalog, without running any of them, and when `check_result` holds, check the final statement against the declared result.
pub fn validate_sql_function_body(
    context: &SQLBodyValidationContext<'_>,
    def: &CreateFunction,
    body: &CompiledFunctionBody,
    check_result: bool,
) -> Result<(), SQLError> {
    let CompiledFunctionBody::SQL(plans) = body else {
        return Ok(());
    };
    if !sql_body_is_analyzable(def) {
        return Ok(());
    }
    let params = routine_parameter_values(context.compilation.types, def);
    let scope = context.compilation.catalog.binding_snapshot()?;
    // A SQL-standard body resolved its parameter names when it was compiled.
    let parameters = matches!(def.body, FunctionBody::Source(_))
        .then(|| sql_body_parameter_scope(def, &params))
        .transpose()?;
    let mut last = None;
    for plan in plans {
        let mut statement = plan.clone();
        if let Some(parameters) = &parameters {
            bind_routine_parameter_references(
                context.compilation.routines,
                &mut statement,
                &params,
                &scope.context(),
                parameters,
            )?;
        }
        reject_undefined_parameters(&mut statement, params.len())?;
        last = Some(analyze_body_statement(
            context, &statement, &params, &scope,
        )?);
    }
    if check_result {
        check_sql_function_result(context.compilation.types, def, last.as_ref())?;
    }
    Ok(())
}

fn analyze_body_statement(
    context: &SQLBodyValidationContext<'_>,
    plan: &UnifiedPlan,
    params: &[SQLParam],
    scope: &dyn StatementBindingScope,
) -> Result<AnalyzedResult, SQLError> {
    if let UnifiedPlan::Command(command) = plan {
        if let CommandPlan::Call { name, args } = command.as_ref() {
            let binding = scope.binding_context()?;
            reject_output_argument_call(
                &context.overloads,
                context.compilation.types,
                name,
                args,
                &mut |argument| {
                    crate::binding::analyze_expression_plan_type(
                        context.compilation.routines,
                        argument,
                        params,
                        &binding,
                    )
                },
            )?;
            return Ok(AnalyzedResult::Command);
        }
    }
    analyze_plan_result(context.compilation.routines, plan, params, scope)
}

/// Resolve the procedure a `CALL` names and reject one with output arguments, which `PostgreSQL` does not support in SQL functions.
pub fn reject_output_argument_call(
    overloads: &RoutineOverloadContext<'_>,
    types: &dyn RoutineTypeCatalog,
    name: &str,
    arguments: &[ExpressionPlan],
    infer: &mut dyn FnMut(&ExpressionPlan) -> Result<Option<ColumnType>, SQLError>,
) -> Result<(), SQLError> {
    if ProcedureCallAnalysis::new(arguments)?
        .result_schema(name, overloads, types, infer)?
        .is_some()
    {
        return Err(SQLError::Routine {
            sqlstate: "0A000".into(),
            message: "calling procedures with output arguments is not supported in SQL functions"
                .into(),
        });
    }
    Ok(())
}

/// Reject a reference to a parameter the routine does not declare, as `PostgreSQL`'s parser reports it.
pub fn reject_undefined_parameters(
    plan: &mut UnifiedPlan,
    declared: usize,
) -> Result<(), SQLError> {
    let mut undefined = None;
    plan.rewrite_scalar_expressions(&mut |expression| {
        if let ScalarExpr::Param(index) = expression {
            if *index > declared && undefined.is_none() {
                undefined = Some(*index);
            }
        }
    });
    match undefined {
        Some(index) => Err(SQLError::Routine {
            sqlstate: "42P02".into(),
            message: format!("there is no parameter ${index}"),
        }),
        None => Ok(()),
    }
}

/// Typed placeholders for the parameters the body names, against which analysis types references to them.
#[must_use]
pub fn routine_parameter_values(
    types: &dyn RoutineTypeCatalog,
    def: &CreateFunction,
) -> Vec<SQLParam> {
    sql_body_parameters(def)
        .iter()
        .map(|parameter| {
            match types
                .resolve_catalog_column_type(&parameter.type_name)
                .or_else(|| ColumnType::from_sql_name(&parameter.type_name).ok())
            {
                Some(ty) => SQLParam::typed_scalar(Value::Null, ty),
                None => SQLParam::scalar(Value::Null),
            }
        })
        .collect()
}
