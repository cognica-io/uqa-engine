//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Routine declarations as `ruleutils.c` prints them: `pg_get_function_arguments`, `pg_get_function_identity_arguments`, `pg_get_function_result` and `pg_get_function_sqlbody`. Types print as `format_type_be` spells them in the current search path.

use uqa_core::Value;
use uqa_sql::ast::{FunctionBody, FunctionParamMode, FunctionReturns, SQLBodyForm, Statement};
use uqa_sql::expr::quote_ident;
use uqa_sql::routines::SQLUserFunction;
use uqa_sql::SQLError;

use crate::catalog::context::CatalogContext;
use crate::catalog::{CatalogReadView, RelationNameResolution};

use super::builtin_routines::{BuiltinRoutineCatalogEntry, PG18_BUILTIN_ROUTINE_GROUPS};

mod builtin_body;

enum Routine {
    User(std::sync::Arc<SQLUserFunction>),
    Builtin(&'static BuiltinRoutineCatalogEntry),
}

/// Preserve the stored default's pseudo-type or unknown input type while
/// reconstructing its expression, without turning it into a concrete carrier.
pub(super) fn routine_parameter_default_text(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    parameter: &uqa_sql::ast::FunctionParam,
) -> Result<Option<String>, SQLError> {
    use uqa_sql::ast::{Expr, RoutineDefaultType};
    let Some(default) = &parameter.default else {
        return Ok(None);
    };
    let typed;
    let expression = if matches!(default, Expr::Literal(Value::Str(_) | Value::Null)) {
        let ty = match &parameter.default_type {
            Some(RoutineDefaultType::Concrete(ty)) => ty.catalog_name(),
            Some(RoutineDefaultType::Polymorphic(name)) => name.clone(),
            None => "unknown".into(),
        };
        typed = Expr::Cast {
            expr: Box::new(default.clone()),
            ty,
        };
        &typed
    } else {
        default
    };
    super::view_definition::stored_expression_text(catalog, resolution, expression).map(Some)
}

fn routine_oid_argument(name: &str, arguments: &[Value]) -> Result<Option<i64>, SQLError> {
    match arguments {
        [Value::Null] => Ok(None),
        [Value::Int(oid)] => Ok(Some(*oid)),
        [_] => Err(SQLError::TypeMismatch(format!("{name} requires an oid"))),
        _ => Err(SQLError::BadArity {
            name: name.into(),
            expected: "1".into(),
            actual: arguments.len(),
        }),
    }
}

fn find_routine(context: &CatalogContext<'_>, oid: i64) -> Result<Option<Routine>, SQLError> {
    for function in context.catalog_read_view().all_sql_functions() {
        if super::pg_proc::user_routine_catalog_oid(&function)? == oid {
            return Ok(Some(Routine::User(function)));
        }
    }
    Ok(PG18_BUILTIN_ROUTINE_GROUPS
        .iter()
        .flat_map(|group| group.iter())
        .find(|entry| entry.oid == oid)
        .map(Routine::Builtin))
}

/// `format_type_be` of a type OID.
fn type_display(context: &CatalogContext<'_>, oid: i64) -> Result<String, SQLError> {
    match super::format_type_value(context, &[Value::Int(oid), Value::Null])? {
        Value::Str(name) => Ok(name),
        other => Err(SQLError::Internal(format!(
            "format_type returned {other:?} for type {oid}"
        ))),
    }
}

/// `format_type_be` of a declared routine type.
fn declared_type_display(
    context: &CatalogContext<'_>,
    type_name: &str,
) -> Result<String, SQLError> {
    let oid = super::regtypes::catalog_routine_type_oid(&context.catalog_read_view(), type_name);
    type_display(context, oid)
}

/// `print_function_arguments`: `TABLE` columns print only in the result; defaults print only in the full argument list.
fn user_arguments(
    context: &CatalogContext<'_>,
    function: &SQLUserFunction,
    table_arguments: bool,
    defaults: bool,
) -> Result<(String, usize), SQLError> {
    let def = &function.def;
    let catalog = context.catalog_read_view();
    let resolution = context.session_execution_view().relation_name_resolution();
    let mut printed = Vec::new();
    for parameter in &def.params {
        let (mode, input) = match parameter.mode {
            // Procedures mark every mode, which keeps the list unambiguous for DROP PROCEDURE.
            FunctionParamMode::In if def.is_procedure => ("IN ", true),
            FunctionParamMode::In => ("", true),
            FunctionParamMode::InOut => ("INOUT ", true),
            FunctionParamMode::Out => ("OUT ", false),
            FunctionParamMode::Variadic => ("VARIADIC ", true),
            FunctionParamMode::Table => ("", false),
        };
        if table_arguments != (parameter.mode == FunctionParamMode::Table) {
            continue;
        }
        // The identity list names only the arguments that select the routine; procedures include their output arguments.
        if !defaults && !input && !def.is_procedure && !table_arguments {
            continue;
        }
        let mut argument = mode.to_string();
        if !parameter.name.is_empty() {
            argument.push_str(&quote_ident(&parameter.name));
            argument.push(' ');
        }
        argument.push_str(&declared_type_display(context, &parameter.type_name)?);
        if defaults && input {
            if let Some(default) = routine_parameter_default_text(&catalog, &resolution, parameter)?
            {
                argument.push_str(" DEFAULT ");
                argument.push_str(&default);
            }
        }
        printed.push(argument);
    }
    Ok((printed.join(", "), printed.len()))
}

fn builtin_arguments(
    context: &CatalogContext<'_>,
    entry: &BuiltinRoutineCatalogEntry,
    defaults: bool,
) -> Result<String, SQLError> {
    let first_default = entry
        .argument_types
        .len()
        .saturating_sub(entry.default_arguments);
    let default_texts = entry
        .argument_defaults
        .map(|defaults| defaults.split(", ").collect::<Vec<_>>())
        .unwrap_or_default();
    let mut printed = Vec::with_capacity(entry.argument_types.len());
    for (index, oid) in entry.argument_types.iter().enumerate() {
        let mut argument = String::new();
        if let Some(name) = entry
            .argument_names
            .get(index)
            .filter(|name| !name.is_empty())
        {
            argument.push_str(&quote_ident(name));
            argument.push(' ');
        }
        argument.push_str(&type_display(context, *oid)?);
        if defaults && index >= first_default {
            if let Some(text) = default_texts.get(index - first_default) {
                argument.push_str(" DEFAULT ");
                argument.push_str(text);
            }
        }
        printed.push(argument);
    }
    Ok(printed.join(", "))
}

pub fn pg_get_function_arguments_value(
    context: &CatalogContext<'_>,
    arguments: &[Value],
) -> Result<Value, SQLError> {
    function_arguments(context, "pg_get_function_arguments", arguments, true)
}

pub fn pg_get_function_identity_arguments_value(
    context: &CatalogContext<'_>,
    arguments: &[Value],
) -> Result<Value, SQLError> {
    function_arguments(
        context,
        "pg_get_function_identity_arguments",
        arguments,
        false,
    )
}

fn function_arguments(
    context: &CatalogContext<'_>,
    name: &str,
    arguments: &[Value],
    defaults: bool,
) -> Result<Value, SQLError> {
    let Some(oid) = routine_oid_argument(name, arguments)? else {
        return Ok(Value::Null);
    };
    Ok(match find_routine(context, oid)? {
        Some(Routine::User(function)) => {
            Value::Str(user_arguments(context, &function, false, defaults)?.0)
        }
        Some(Routine::Builtin(entry)) => Value::Str(builtin_arguments(context, entry, defaults)?),
        None => Value::Null,
    })
}

/// `print_function_rettype`; procedures have no result.
pub fn pg_get_function_result_value(
    context: &CatalogContext<'_>,
    arguments: &[Value],
) -> Result<Value, SQLError> {
    let Some(oid) = routine_oid_argument("pg_get_function_result", arguments)? else {
        return Ok(Value::Null);
    };
    let Some(routine) = find_routine(context, oid)? else {
        return Ok(Value::Null);
    };
    let function = match routine {
        Routine::Builtin(entry) => {
            return if entry.kind == "p" {
                Ok(Value::Null)
            } else {
                type_display(context, entry.return_type).map(Value::Str)
            };
        }
        Routine::User(function) => function,
    };
    let def = &function.def;
    if def.is_procedure {
        return Ok(Value::Null);
    }
    if def.returns_set() {
        let (columns, count) = user_arguments(context, &function, true, false)?;
        if count > 0 {
            return Ok(Value::Str(format!("TABLE({columns})")));
        }
    }
    let result = match &def.returns {
        FunctionReturns::Scalar { type_name } | FunctionReturns::SetOf { type_name } => {
            declared_type_display(context, type_name)?
        }
        FunctionReturns::None | FunctionReturns::Table => match def.output_params().as_slice() {
            [output] => declared_type_display(context, &output.type_name)?,
            [] => "void".into(),
            _ => "record".into(),
        },
    };
    Ok(Value::Str(if def.returns_set() {
        format!("SETOF {result}")
    } else {
        result
    }))
}

/// `print_function_sqlbody`: a `RETURN` body prints its expression; a `BEGIN ATOMIC` body prints each statement as a query definition.
pub fn pg_get_function_sqlbody_value(
    context: &CatalogContext<'_>,
    arguments: &[Value],
) -> Result<Value, SQLError> {
    let Some(oid) = routine_oid_argument("pg_get_function_sqlbody", arguments)? else {
        return Ok(Value::Null);
    };
    let function = match find_routine(context, oid)? {
        Some(Routine::User(function)) => function,
        Some(Routine::Builtin(routine)) => return builtin_body::definition(context, routine),
        None => return Ok(Value::Null),
    };
    let FunctionBody::Statements(statements) = &function.def.body else {
        return Ok(Value::Null);
    };
    let catalog = context.catalog_read_view();
    let resolution = context.session_execution_view().relation_name_resolution();
    let form = function
        .def
        .sql_body_form
        .unwrap_or_else(|| legacy_body_form(statements));
    super::view_definition::routine_body_definition(
        &catalog,
        &resolution,
        &function.def,
        form,
        statements,
    )
    .map(Value::Str)
}

/// Definitions stored before the body form was recorded: a body of one `SELECT` of one unnamed value without clauses is the stored form of `RETURN`.
fn legacy_body_form(statements: &[Statement]) -> SQLBodyForm {
    match statements {
        [Statement::Select(select)]
            if select.from.is_none()
                && select.with.is_empty()
                && select.values.is_empty()
                && select.set_op.is_none()
                && select.r#where.is_none()
                && select.group_by.is_empty()
                && select.having.is_none()
                && select.order_by.is_empty()
                && select.limit.is_none()
                && select.offset.is_none()
                && !select.distinct
                && matches!(select.projections.as_slice(), [projection] if projection.alias.is_none()) =>
        {
            SQLBodyForm::Return
        }
        _ => SQLBodyForm::Atomic,
    }
}
