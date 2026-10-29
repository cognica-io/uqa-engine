//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::{borrow::Cow, cell::RefCell};

pub mod inputs;
mod parameters;
mod statements;

use super::descriptions::RoutineBodyDescriptions;
use super::transaction::DirectRoutineCommandGuard;
use super::{CreateFunction, RoutineContext, RoutineOutcome, SQLError, SQLParam, SQLResult, Value};
use uqa_sql::{
    assignment::routines::coerce_routine_value_from,
    ast::FunctionBody,
    binding::statements::AnalyzedResult,
    plan::{CommandPlan, UnifiedPlan},
    plpgsql::runtime_diagnostics::result_row_values,
    routines::{
        body_parameters::sql_body_parameter_scope_with_control,
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
    execute_described_sql_language(context, types, overloads, body, bound, record_target, None)
}

pub(crate) fn execute_described_sql_language(
    context: RoutineContext<'_>,
    types: &dyn RoutineTypeCatalog,
    overloads: &RoutineOverloadContext<'_>,
    body: SQLBody<'_>,
    bound: &[Value],
    record_target: Option<&[uqa_sql::routines::result_check::SQLFunctionResultColumn]>,
    descriptions: Option<RoutineBodyDescriptions<'_>>,
) -> Result<RoutineOutcome, SQLError> {
    let SQLBody {
        function,
        definition: def,
        plans,
    } = body;
    let resources = context.planning_resources()?;
    let control = resources.control();
    let params = parameters::body_parameters_with_control(
        context.expressions,
        def,
        bound,
        descriptions,
        &control,
    )?;
    if plans.is_empty() {
        check_sql_function_result(types, def, None)?;
    }
    let layout = RefCell::new(None);
    let check_result = |result: &AnalyzedResult| {
        *layout.borrow_mut() = Some(checked_body_result_layout(
            types,
            def,
            result,
            record_target,
            descriptions,
        )?);
        Ok(())
    };
    let parameters = matches!(def.body, FunctionBody::Source(_))
        .then(|| sql_body_parameter_scope_with_control(def, &params, &control))
        .transpose()?;
    let identity = inputs::SQLBodyIdentity::new(function)?;
    let mut parameter_types = uqa_core::memory::ProductionVec::new(control);
    parameter_types.reserve(params.len())?;
    for parameter in params.iter() {
        let ty = parameter
            .declared_scalar_type()
            .ok_or_else(|| SQLError::Internal("SQL body parameter has no concrete type".into()))?;
        parameter_types.push_produced(ty.clone_with_control(&control)?)?;
    }
    let parameter_types = parameter_types.finish()?;
    let statements = statements::SQLStatements {
        control,
        context,
        types,
        overloads,
        params: &params,
        parameters,
        identity,
        parameter_types: &parameter_types,
        inputs: context.statements.body_input_context(),
    };
    let mut last = SQLResult::empty();
    for (position, plan) in plans.iter().enumerate() {
        control.check()?;
        let check = (position + 1 == plans.len())
            .then_some(&check_result as super::context::StatementResultCheck<'_>);
        let frame = || {
            uqa_sql::routines::diagnostics::sql_context(
                def,
                uqa_sql::routines::diagnostics::SQLRoutineStage::Statement(position + 1),
            )
        };
        let statement = statements
            .prepare(plan, position, check)
            .map_err(|error| def.source_body_error(error).with_context(frame()))?;
        let _direct_routine_command = matches!(
            statement.plan(),
            UnifiedPlan::Command(command)
                if matches!(
                    command.as_ref(),
                    CommandPlan::Call { .. } | CommandPlan::DoBlock { .. }
                )
        )
        .then(|| DirectRoutineCommandGuard::enter(context.session));
        last = statement
            .execute(context, &params, check)
            .map_err(|error| def.source_body_error(error).with_context(frame()))?;
    }
    let layout = layout.into_inner().map_or_else(
        || sql_function_result_layout(types, def, None).map(Cow::Owned),
        Ok,
    )?;
    shape_result(
        context.expressions,
        types,
        def,
        &last,
        &layout,
        descriptions,
    )
    .map_err(|error| {
        error.with_context(uqa_sql::routines::diagnostics::sql_context(
            def,
            uqa_sql::routines::diagnostics::SQLRoutineStage::Statement(plans.len()),
        ))
    })
}

fn checked_body_result_layout<'a>(
    types: &dyn RoutineTypeCatalog,
    definition: &CreateFunction,
    result: &AnalyzedResult,
    record_target: Option<&[uqa_sql::routines::result_check::SQLFunctionResultColumn]>,
    descriptions: Option<RoutineBodyDescriptions<'a>>,
) -> Result<Cow<'a, SQLFunctionResultLayout>, SQLError> {
    let layout = match descriptions {
        Some(saved) => saved.description.body_result_layout(
            types,
            definition,
            Some(result),
            saved.environment,
        )?,
        None => Cow::Owned(sql_function_result_layout(types, definition, Some(result))?),
    };
    if layout.kind == SQLFunctionResultKind::Tuple && routine_returns_anonymous_record(definition) {
        if let Some(target) = record_target {
            uqa_sql::routines::result_check::validate_anonymous_record_result(
                types,
                result.column_types().unwrap_or_default(),
                target,
                Some(SQLFunctionResultKind::Tuple),
            )?;
        }
    }
    Ok(layout)
}

#[cfg(test)]
fn body_parameters(
    expressions: &dyn uqa_sql::assignment::routines::RoutineValueContext,
    def: &CreateFunction,
    bound: &[Value],
    descriptions: Option<RoutineBodyDescriptions<'_>>,
) -> Result<Vec<SQLParam>, SQLError> {
    parameters::body_parameters_with_control(
        expressions,
        def,
        bound,
        descriptions,
        &uqa_core::memory::ProductionControl::uncontrolled(),
    )
    .map(|parameters| {
        parameters
            .into_uncontrolled()
            .expect("ordinary SQL-body parameters")
    })
}

fn shape_result(
    expressions: &dyn uqa_sql::assignment::routines::RoutineValueContext,
    types: &dyn RoutineTypeCatalog,
    def: &CreateFunction,
    last: &SQLResult,
    layout: &SQLFunctionResultLayout,
    descriptions: Option<RoutineBodyDescriptions<'_>>,
) -> Result<RoutineOutcome, SQLError> {
    let output_count = def.output_arity();
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
    let shape = |values| result_value(expressions, types, layout, last, values, descriptions);
    if def.returns_set() {
        if layout.kind == SQLFunctionResultKind::Value {
            uqa_sql::routines::result_check::validate_sql_function_record_rows(last)?;
        }
        let mut rows = Vec::with_capacity(last.rows.len());
        for index in 0..last.rows.len() {
            let value = shape(result_row_values(last, index).unwrap_or_default())?;
            rows.push(output_values(def, value, output_count, Some(expressions))?);
        }
        return Ok(RoutineOutcome {
            value: Value::Null,
            out_values: vec![Value::Null; output_count],
            set_rows: rows,
            anonymous_record_column_types: record_types,
            sql_result_kind: Some(layout.kind),
        });
    }
    let value = result_row_values(last, 0)
        .map(shape)
        .transpose()?
        .unwrap_or(Value::Null);
    let (value, out_values) = if output_count == 0 {
        (value, Vec::new())
    } else {
        (
            Value::Null,
            output_values(def, value, output_count, Some(expressions))?,
        )
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
    descriptions: Option<RoutineBodyDescriptions<'_>>,
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
                    let fields = row_fields(value, expected.len(), Some(expressions))?;
                    return Ok(Value::Record(
                        expected
                            .iter()
                            .map(|column| column.name.clone())
                            .zip(fields)
                            .collect::<Vec<_>>()
                            .into(),
                    ));
                }
            }
            let source = last.column_types.first().and_then(Option::as_ref);
            match descriptions {
                Some(saved) => saved.description.coerce_body_result(
                    layout,
                    0,
                    &value,
                    source,
                    saved.environment,
                ),
                None => coerce_routine_value_from(
                    expressions,
                    &value,
                    &layout.declared_type.catalog_name(),
                    source,
                ),
            }
        }
        SQLFunctionResultKind::Tuple => {
            let columns = if let Some(expected) = &layout.columns {
                for (index, (value, column)) in values.iter_mut().zip(expected).enumerate() {
                    let source = last.column_types.get(index).and_then(Option::as_ref);
                    *value = match descriptions {
                        Some(saved) => saved.description.coerce_body_result(
                            layout,
                            index,
                            value,
                            source,
                            saved.environment,
                        ),
                        None => coerce_routine_value_from(
                            expressions,
                            value,
                            &column.ty.catalog_name(),
                            source,
                        ),
                    }?;
                }
                expected
                    .iter()
                    .map(|column| column.name.clone())
                    .collect::<Vec<_>>()
            } else {
                last.columns.clone()
            };
            Ok(Value::Record(
                columns.into_iter().zip(values).collect::<Vec<_>>().into(),
            ))
        }
    }
}

/// A single composite OUT parameter is a value for a function, but remains one independently assigned output column for a procedure.
fn output_values(
    def: &CreateFunction,
    value: Value,
    output_count: usize,
    engine: Option<&dyn uqa_sql::expr::EngineHook>,
) -> Result<Vec<Value>, SQLError> {
    if output_count == 0 || (output_count == 1 && !def.is_procedure) {
        Ok(vec![value])
    } else {
        row_fields(value, output_count, engine)
    }
}

pub(super) fn row_fields(
    value: Value,
    expected: usize,
    engine: Option<&dyn uqa_sql::expr::EngineHook>,
) -> Result<Vec<Value>, SQLError> {
    let value = match value {
        Value::Datum(datum) => uqa_sql::expr::datums::read_with_catalog(&datum, engine)?,
        value => value,
    };
    let fields = match value {
        Value::Record(fields) => fields.into_values(),
        Value::Row(fields) => fields.into_values(),
        Value::Null => return Ok(vec![Value::Null; expected]),
        _ => {
            return Err(SQLError::Internal(
                "a SQL routine row result contained a scalar value".into(),
            ));
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
