//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::transaction::DirectRoutineCommandGuard;
use super::{
    CreateFunction, FunctionReturns, RoutineContext, RoutineOutcome, SQLError, SQLParam, SQLResult,
    Value,
};
use uqa_sql::{
    assignment::routines::coerce_routine_value_from,
    ast::FunctionBody,
    binding::statements::AnalyzedResult,
    plan::{CommandPlan, UnifiedPlan},
    plpgsql::runtime_diagnostics::result_row_values,
    routines::{
        body_parameters::{
            is_sql_body_parameter, resolve_sql_body_parameters, sql_body_parameter_scope,
        },
        body_validation::{reject_output_argument_call, reject_undefined_parameters},
        compilation::RoutineCompilationContext,
        resolution::RoutineOverloadContext,
        result_check::check_sql_function_result,
        routine_returns_anonymous_record,
    },
};

/// `LANGUAGE sql` body: run every statement, each analyzed just before it runs as `PostgreSQL` analyzes them, with the final statement checked against the declared result before it runs; the last statement's result shapes the routine output. A body given as a string resolves the names of its parameters in each statement when that statement is analyzed.
#[expect(clippy::too_many_lines, reason = "preserves PL/pgSQL transition order")]
pub fn execute_sql_language(
    context: RoutineContext<'_>,
    compilation: &RoutineCompilationContext<'_>,
    overloads: &RoutineOverloadContext<'_>,
    def: &CreateFunction,
    plans: &[UnifiedPlan],
    bound: &[Value],
) -> Result<RoutineOutcome, SQLError> {
    let types = compilation.types;
    let call_params = def.call_params();
    if call_params.len() != bound.len() {
        return Err(SQLError::Internal(format!(
            "routine `{}` received {} values for {} concrete call parameters",
            def.name,
            bound.len(),
            call_params.len()
        )));
    }
    let params = bound
        .iter()
        .cloned()
        .zip(call_params)
        .filter(|(_, parameter)| is_sql_body_parameter(parameter))
        .map(|(value, parameter)| {
            let ty = uqa_sql::ast::ColumnType::from_sql_name(&parameter.type_name)
                .ok()
                .or_else(|| {
                    context
                        .expressions
                        .catalog_column_type(&parameter.type_name)
                })
                .ok_or_else(|| {
                    SQLError::TypeMismatch(format!("unknown type `{}`", parameter.type_name))
                })?;
            Ok(SQLParam::typed_scalar(value, ty))
        })
        .collect::<Result<Vec<_>, SQLError>>()?;
    if plans.is_empty() {
        check_sql_function_result(types, def, None)?;
    }
    let check_result =
        |result: &AnalyzedResult| check_sql_function_result(types, def, Some(result));
    let parameters = matches!(def.body, FunctionBody::Source(_))
        .then(|| sql_body_parameter_scope(def, &params))
        .transpose()?;
    let mut last = SQLResult::empty();
    for (position, plan) in plans.iter().enumerate() {
        let mut statement = plan.clone();
        if let Some(parameters) = &parameters {
            resolve_sql_body_parameters(compilation, parameters, &mut statement, &params)?;
        }
        reject_undefined_parameters(&mut statement, params.len())?;
        if let UnifiedPlan::Command(command) = &statement {
            if let CommandPlan::Call { name, args } = command.as_ref() {
                reject_output_argument_call(overloads, types, name, args, &mut |argument| {
                    context.expressions.expression_type(argument, &params)
                })?;
            }
        }
        let _direct_routine_command = matches!(
            &statement,
            UnifiedPlan::Command(command)
                if matches!(
                    command.as_ref(),
                    CommandPlan::Call { .. } | CommandPlan::DoBlock { .. }
                )
        )
        .then(|| DirectRoutineCommandGuard::enter(context.session));
        let check = (position + 1 == plans.len())
            .then_some(&check_result as super::context::StatementResultCheck<'_>);
        last = context
            .statements
            .execute_body_statement(statement, &params, check)?;
    }
    let out_params = def.output_params();
    let returns_anonymous_record = routine_returns_anonymous_record(def);
    let returns_void = matches!(
        &def.returns,
        FunctionReturns::Scalar { type_name } if type_name == "void"
    );
    let expected = if out_params.is_empty() {
        1
    } else {
        out_params.len()
    };
    if def.returns_set() {
        let mut set_rows = Vec::with_capacity(last.rows.len());
        for row_index in 0..last.rows.len() {
            let values = result_row_values(&last, row_index).unwrap_or_default();
            if returns_anonymous_record {
                set_rows.push(vec![anonymous_record_value(&last.columns, values)]);
                continue;
            }
            let (mut values, expanded) = expand_lone_row(values, expected)?;
            if out_params.is_empty() {
                if let FunctionReturns::SetOf { type_name } = &def.returns {
                    values[0] = coerce_routine_value_from(
                        context.expressions,
                        &values[0],
                        type_name,
                        last.column_types.first().and_then(Option::as_ref),
                    )?;
                }
            } else {
                for (index, (value, parameter)) in values.iter_mut().zip(&out_params).enumerate() {
                    let source = if expanded {
                        None
                    } else {
                        last.column_types.get(index).and_then(Option::as_ref)
                    };
                    *value = coerce_routine_value_from(
                        context.expressions,
                        value,
                        &parameter.type_name,
                        source,
                    )?;
                }
            }
            set_rows.push(values);
        }
        return Ok(RoutineOutcome {
            value: Value::Null,
            out_values: vec![Value::Null; out_params.len()],
            set_rows,
            anonymous_record_column_types: returns_anonymous_record
                .then(|| last.column_types.clone()),
        });
    }
    let first = result_row_values(&last, 0);
    if !out_params.is_empty() {
        let mut out_values = vec![Value::Null; out_params.len()];
        if let Some(values) = first {
            let (values, expanded) = expand_lone_row(values, expected)?;
            for (idx, value) in values.into_iter().take(out_values.len()).enumerate() {
                let source = if expanded {
                    None
                } else {
                    last.column_types.get(idx).and_then(Option::as_ref)
                };
                out_values[idx] = coerce_routine_value_from(
                    context.expressions,
                    &value,
                    &out_params[idx].type_name,
                    source,
                )?;
            }
        }
        return Ok(RoutineOutcome {
            value: Value::Null,
            out_values,
            set_rows: Vec::new(),
            anonymous_record_column_types: None,
        });
    }
    let value = match first {
        Some(_) if returns_void => Value::Null,
        Some(values) if returns_anonymous_record => anonymous_record_value(&last.columns, values),
        Some(mut values) => {
            if values.is_empty() {
                Value::Null
            } else {
                let value = values.remove(0);
                match &def.returns {
                    FunctionReturns::Scalar { type_name } => coerce_routine_value_from(
                        context.expressions,
                        &value,
                        type_name,
                        last.column_types.first().and_then(Option::as_ref),
                    )?,
                    _ => value,
                }
            }
        }
        None => Value::Null,
    };
    Ok(RoutineOutcome {
        value,
        out_values: Vec::new(),
        set_rows: Vec::new(),
        anonymous_record_column_types: returns_anonymous_record.then(|| last.column_types.clone()),
    })
}

fn anonymous_record_value(columns: &[String], values: Vec<Value>) -> Value {
    Value::Record(columns.iter().cloned().zip(values).collect())
}

/// The output columns of a result row, and whether they came from a row value: a lone row value that the final statement returns for several output columns is the whole result, and its fields fill the columns in order, as `PostgreSQL` returns it.
fn expand_lone_row(
    mut values: Vec<Value>,
    expected: usize,
) -> Result<(Vec<Value>, bool), SQLError> {
    if expected < 2 || values.len() != 1 {
        return Ok((values, false));
    }
    let fields = match values.remove(0) {
        Value::Row(fields) => fields,
        Value::Record(fields) => fields.into_iter().map(|(_, value)| value).collect(),
        Value::Null => return Ok((vec![Value::Null; expected], true)),
        other => {
            return Err(SQLError::Internal(format!(
                "the lone row column of a SQL function result held {other:?}"
            )))
        }
    };
    if fields.len() != expected {
        return Err(SQLError::Diagnostic {
            sqlstate: "42804".into(),
            message: "function return row and query-specified return row do not match".into(),
            detail: Some(format!(
                "Returned row contains {} attribute{}, but query expects {expected}.",
                fields.len(),
                if fields.len() == 1 { "" } else { "s" }
            )),
            hint: None,
        });
    }
    Ok((fields, true))
}
