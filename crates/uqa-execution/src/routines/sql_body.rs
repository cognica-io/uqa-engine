//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::cell::RefCell;

pub mod inputs;
mod statements;

use super::transaction::DirectRoutineCommandGuard;
use super::{CreateFunction, RoutineContext, RoutineOutcome, SQLError, SQLParam, SQLResult, Value};
use uqa_sql::{
    assignment::routines::coerce_routine_value_from,
    ast::FunctionBody,
    binding::statements::AnalyzedResult,
    plan::{CommandPlan, UnifiedPlan},
    plpgsql::runtime_diagnostics::result_row_values,
    routines::{
        body_parameters::{is_sql_body_parameter, sql_body_parameter_scope},
        declaration::RoutineTypeCatalog,
        resolution::RoutineOverloadContext,
        result_check::{
            check_sql_function_result, sql_function_result_layout, validate_sql_function_record,
            SQLFunctionResultKind, SQLFunctionResultLayout,
        },
        routine_returns_anonymous_record, SQLUserFunction,
    },
};

pub struct SQLBody<'a> {
    pub function: &'a SQLUserFunction,
    pub definition: &'a CreateFunction,
    pub plans: &'a [UnifiedPlan],
}

/// `LANGUAGE sql` statements retain their first successful analysis until the
/// routine, concrete input types, namespace or selected dependencies change.
/// The final statement is checked before execution and shapes the routine output.
pub fn execute_sql_language(
    context: RoutineContext<'_>,
    types: &dyn RoutineTypeCatalog,
    overloads: &RoutineOverloadContext<'_>,
    body: SQLBody<'_>,
    bound: &[Value],
    record_target: Option<&[uqa_sql::routines::result_check::SQLFunctionResultColumn]>,
) -> Result<RoutineOutcome, SQLError> {
    let SQLBody {
        function,
        definition: def,
        plans,
    } = body;
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
    let layout = RefCell::new(None);
    let check_result = |result: &AnalyzedResult| {
        let result_layout = sql_function_result_layout(types, def, Some(result))?;
        if result_layout.kind == SQLFunctionResultKind::Tuple
            && routine_returns_anonymous_record(def)
        {
            if let Some(target) = record_target {
                uqa_sql::routines::result_check::validate_anonymous_record_result(
                    types,
                    result.column_types().unwrap_or_default(),
                    target,
                    Some(SQLFunctionResultKind::Tuple),
                )?;
            }
        }
        *layout.borrow_mut() = Some(result_layout);
        Ok(())
    };
    let parameters = matches!(def.body, FunctionBody::Source(_))
        .then(|| sql_body_parameter_scope(def, &params))
        .transpose()?;
    let identity = inputs::SQLBodyIdentity::new(function)?;
    let parameter_types = params
        .iter()
        .map(|param| {
            param
                .declared_scalar_type()
                .cloned()
                .ok_or_else(|| SQLError::Internal("SQL body parameter has no concrete type".into()))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let statements = statements::SQLStatements {
        context,
        types,
        overloads,
        params: &params,
        parameters,
        identity,
        parameter_types,
        inputs: context.statements.body_input_context(),
    };
    let mut last = SQLResult::empty();
    for (position, plan) in plans.iter().enumerate() {
        let check = (position + 1 == plans.len())
            .then_some(&check_result as super::context::StatementResultCheck<'_>);
        let statement = statements.prepare(plan, position, check)?;
        let _direct_routine_command = matches!(
            &statement,
            UnifiedPlan::Command(command)
                if matches!(
                    command.as_ref(),
                    CommandPlan::Call { .. } | CommandPlan::DoBlock { .. }
                )
        )
        .then(|| DirectRoutineCommandGuard::enter(context.session));
        last = context
            .statements
            .execute_body_statement(statement, &params, check)?;
    }
    let layout = layout
        .into_inner()
        .map_or_else(|| sql_function_result_layout(types, def, None), Ok)?;
    shape_result(context.expressions, types, def, &last, &layout)
}

fn shape_result(
    expressions: &dyn uqa_sql::assignment::routines::RoutineValueContext,
    types: &dyn RoutineTypeCatalog,
    def: &CreateFunction,
    last: &SQLResult,
    layout: &SQLFunctionResultLayout,
) -> Result<RoutineOutcome, SQLError> {
    let outputs = def.output_params();
    let anonymous = routine_returns_anonymous_record(def);
    let record_types = anonymous
        .then(|| {
            if layout.kind == SQLFunctionResultKind::Value {
                layout.source_record.clone()
            } else {
                Some(last.column_types.clone())
            }
        })
        .flatten();
    let shape = |values| result_value(expressions, types, layout, last, values);
    if def.returns_set() {
        if layout.kind == SQLFunctionResultKind::Value {
            uqa_sql::routines::result_check::validate_sql_function_record_rows(last)?;
        }
        let mut rows = Vec::with_capacity(last.rows.len());
        for index in 0..last.rows.len() {
            let value = shape(result_row_values(last, index).unwrap_or_default())?;
            rows.push(output_values(def, value, outputs.len())?);
        }
        return Ok(RoutineOutcome {
            value: Value::Null,
            out_values: vec![Value::Null; outputs.len()],
            set_rows: rows,
            anonymous_record_column_types: record_types,
            sql_result_kind: Some(layout.kind),
        });
    }
    let value = result_row_values(last, 0)
        .map(shape)
        .transpose()?
        .unwrap_or(Value::Null);
    let (value, out_values) = if outputs.is_empty() {
        (value, Vec::new())
    } else {
        (Value::Null, output_values(def, value, outputs.len())?)
    };
    Ok(RoutineOutcome {
        value,
        out_values,
        set_rows: Vec::new(),
        anonymous_record_column_types: record_types,
        sql_result_kind: Some(layout.kind),
    })
}

fn result_value(
    expressions: &dyn uqa_sql::assignment::routines::RoutineValueContext,
    types: &dyn RoutineTypeCatalog,
    layout: &SQLFunctionResultLayout,
    last: &SQLResult,
    mut values: Vec<Value>,
) -> Result<Value, SQLError> {
    match layout.kind {
        SQLFunctionResultKind::Void => Ok(Value::Null),
        SQLFunctionResultKind::Value => {
            let value = values.pop().unwrap_or(Value::Null);
            if layout.declared_type == uqa_sql::ColumnType::Record {
                if let Some(expected) = &layout.columns {
                    if !matches!(value, Value::Null) {
                        let target = expected
                            .iter()
                            .map(|column| column.ty.clone())
                            .collect::<Vec<_>>();
                        if let Value::Row(row) = &value {
                            if let Some(source) = row.field_types() {
                                uqa_sql::routines::result_check::validate_sql_function_record_identity(types, source, &target)?;
                            } else if let Some(source) = &layout.source_record {
                                validate_sql_function_record(types, source, &target)?;
                            }
                        } else if let Some(source) = &layout.source_record {
                            validate_sql_function_record(types, source, &target)?;
                        }
                    }
                    let fields = row_fields(value, expected.len())?;
                    return Ok(Value::Record(
                        expected
                            .iter()
                            .map(|column| column.name.clone())
                            .zip(fields)
                            .collect(),
                    ));
                }
            }
            coerce_routine_value_from(
                expressions,
                &value,
                &layout.declared_type.catalog_name(),
                last.column_types.first().and_then(Option::as_ref),
            )
        }
        SQLFunctionResultKind::Tuple => {
            let columns = if let Some(expected) = &layout.columns {
                for (index, (value, column)) in values.iter_mut().zip(expected).enumerate() {
                    *value = coerce_routine_value_from(
                        expressions,
                        value,
                        &column.ty.catalog_name(),
                        last.column_types.get(index).and_then(Option::as_ref),
                    )?;
                }
                expected
                    .iter()
                    .map(|column| column.name.clone())
                    .collect::<Vec<_>>()
            } else {
                last.columns.clone()
            };
            Ok(Value::Record(columns.into_iter().zip(values).collect()))
        }
    }
}

/// A single composite OUT parameter is a value for a function, but remains one independently assigned output column for a procedure.
fn output_values(
    def: &CreateFunction,
    value: Value,
    output_count: usize,
) -> Result<Vec<Value>, SQLError> {
    if output_count == 0 || (output_count == 1 && !def.is_procedure) {
        Ok(vec![value])
    } else {
        row_fields(value, output_count)
    }
}

pub(super) fn row_fields(value: Value, expected: usize) -> Result<Vec<Value>, SQLError> {
    let fields = match value {
        Value::Record(fields) => fields
            .into_iter()
            .map(|(_, value)| value)
            .collect::<Vec<_>>(),
        Value::Row(fields) => fields.into_values(),
        Value::Null => return Ok(vec![Value::Null; expected]),
        _ => {
            return Err(SQLError::Internal(
                "a SQL routine row result contained a scalar value".into(),
            ))
        }
    };
    if fields.len() == expected {
        Ok(fields)
    } else {
        Err(SQLError::Diagnostic {
            sqlstate: "42804".into(),
            message: "function return row and query-specified return row do not match".into(),
            detail: Some(format!(
                "Returned row contains {} attribute{}, but query expects {expected}.",
                fields.len(),
                if fields.len() == 1 { "" } else { "s" }
            )),
            hint: None,
        })
    }
}

#[cfg(test)]
mod tests;
