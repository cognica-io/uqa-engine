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
    plpgsql::runtime_diagnostics::result_row_values, routines::routine_returns_anonymous_record,
};

/// `LANGUAGE sql` body: run every statement; the last statement's
/// result shapes the routine output.
#[expect(clippy::too_many_lines, reason = "preserves PL/pgSQL transition order")]
pub fn execute_sql_language(
    context: RoutineContext<'_>,
    def: &CreateFunction,
    plans: &[uqa_sql::plan::UnifiedPlan],
    bound: &[Value],
) -> Result<RoutineOutcome, SQLError> {
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
    let mut last = SQLResult::empty();
    for plan in plans {
        let _direct_routine_command = matches!(
            plan,
            uqa_sql::plan::UnifiedPlan::Command(command)
                if matches!(
                    command.as_ref(),
                    uqa_sql::plan::CommandPlan::Call { .. }
                        | uqa_sql::plan::CommandPlan::DoBlock { .. }
                )
        )
        .then(|| DirectRoutineCommandGuard::enter(context.session));
        last = context.statements.execute_plan(plan, &params)?;
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
    // PostgreSQL enforces the final statement's column shape at
    // CREATE time; the engine has no schema binding there, so the
    // same 42P13 error surfaces on the first call instead.
    let shape_checked =
        !returns_void && !returns_anonymous_record && (!def.is_procedure || !out_params.is_empty());
    if shape_checked && last.columns.len() != expected {
        return Err(sql_body_shape_error(def));
    }
    if def.returns_set() {
        let mut set_rows = Vec::with_capacity(last.rows.len());
        for row_index in 0..last.rows.len() {
            let mut values = result_row_values(&last, row_index).unwrap_or_default();
            if !returns_anonymous_record && values.len() != expected {
                return Err(sql_body_shape_error(def));
            }
            if returns_anonymous_record {
                values = vec![anonymous_record_value(&last.columns, values)];
            } else if out_params.is_empty() {
                if let FunctionReturns::SetOf { type_name } = &def.returns {
                    values[0] = coerce_routine_value_from(
                        context.expressions,
                        &values[0],
                        type_name,
                        last.column_types.first().and_then(Option::as_ref),
                    )?;
                }
            } else {
                for ((value, parameter), source) in
                    values.iter_mut().zip(&out_params).zip(&last.column_types)
                {
                    *value = coerce_routine_value_from(
                        context.expressions,
                        value,
                        &parameter.type_name,
                        source.as_ref(),
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
            for (idx, value) in values.into_iter().take(out_values.len()).enumerate() {
                out_values[idx] = coerce_routine_value_from(
                    context.expressions,
                    &value,
                    &out_params[idx].type_name,
                    last.column_types.get(idx).and_then(Option::as_ref),
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

fn sql_body_shape_error(def: &CreateFunction) -> SQLError {
    let declared = match &def.returns {
        FunctionReturns::Scalar { type_name } | FunctionReturns::SetOf { type_name } => {
            type_name.clone()
        }
        FunctionReturns::Table | FunctionReturns::None => "record".into(),
    };
    SQLError::Routine {
        sqlstate: "42P13".into(),
        message: format!("return type mismatch in function declared to return {declared}"),
    }
}
