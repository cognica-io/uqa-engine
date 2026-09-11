//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Routine invocation metadata, concrete signatures, and anonymous-record assignment rules.
use crate::{
    assignment::routines::RoutineValueContext,
    ast::{
        ColumnType, CreateFunction, FunctionParamMode, FunctionReturns, RoutineInvocationBinding,
    },
    expr::value_type_name,
    routines::declaration::RoutineTypeCatalog,
    type_resolution::canonical_routine_type_name,
    SQLError,
};
use uqa_core::Value;
pub fn output_column_names(def: &CreateFunction) -> Vec<String> {
    def.output_params()
        .iter()
        .enumerate()
        .map(|(idx, p)| {
            if p.name.is_empty() {
                format!("column{}", idx + 1)
            } else {
                p.name.clone()
            }
        })
        .collect()
}

pub fn call_signature(name: &str, args: &[(Option<String>, Value)]) -> String {
    let types = args
        .iter()
        .map(|(arg_name, value)| match arg_name {
            Some(arg_name) => format!("{arg_name} => {}", value_type_name(value)),
            None => value_type_name(value).to_string(),
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!("{name}({types})")
}

pub fn routine_resolution_error(
    kind: &str,
    name: &str,
    args: &[(Option<String>, Value)],
    suffix: &str,
) -> SQLError {
    SQLError::Routine {
        sqlstate: if suffix == "is not unique" {
            "42725".into()
        } else {
            "42883".into()
        },
        message: format!("{kind} {} {suffix}", call_signature(name, args)),
    }
}

pub fn runtime_argument_types(
    args: &[(Option<String>, Value)],
) -> Result<Vec<Option<ColumnType>>, SQLError> {
    args.iter()
        .map(|(_, value)| {
            if matches!(value, Value::Null) {
                Ok(None)
            } else {
                ColumnType::from_sql_name(value_type_name(value)).map(Some)
            }
        })
        .collect()
}

pub fn specialized_definition(
    definition: &CreateFunction,
    invocation: &RoutineInvocationBinding,
) -> Result<Option<CreateFunction>, SQLError> {
    if invocation.parameter_types.len() != definition.params.len() {
        return Err(SQLError::Internal(format!(
            "routine `{}` has {} concrete parameter types for {} parameters",
            definition.name,
            invocation.parameter_types.len(),
            definition.params.len()
        )));
    }
    let parameters_match = definition
        .params
        .iter()
        .zip(&invocation.parameter_types)
        .all(|(parameter, type_name)| parameter.type_name == *type_name);
    let return_type_matches = match (&invocation.return_type, &definition.returns) {
        (Some(concrete), FunctionReturns::Scalar { type_name })
        | (Some(concrete), FunctionReturns::SetOf { type_name }) => concrete == type_name,
        (None, _) | (Some(_), FunctionReturns::None | FunctionReturns::Table) => true,
    };
    if parameters_match && return_type_matches {
        return Ok(None);
    }
    let mut specialized = definition.clone();
    for (parameter, type_name) in specialized
        .params
        .iter_mut()
        .zip(&invocation.parameter_types)
    {
        parameter.type_name.clone_from(type_name);
    }
    if let Some(return_type) = &invocation.return_type {
        match &mut specialized.returns {
            FunctionReturns::Scalar { type_name } | FunctionReturns::SetOf { type_name } => {
                type_name.clone_from(return_type);
            }
            FunctionReturns::None | FunctionReturns::Table => {}
        }
    }
    Ok(Some(specialized))
}

pub fn validate_anonymous_record_column_types(
    source_types: &[Option<crate::ast::ColumnType>],
    target_types: &[String],
) -> Result<(), SQLError> {
    if source_types.len() != target_types.len() {
        return Err(anonymous_record_shape_error());
    }
    for (source, target) in source_types.iter().zip(target_types) {
        let Some(source) = source else {
            continue;
        };
        let source = crate::type_resolution::canonical_column_type_name(source);
        let target = canonical_routine_type_name(target);
        if !crate::type_resolution::routine_type_accepts_implicit_cast(&source, &target) {
            return Err(anonymous_record_shape_error());
        }
    }
    Ok(())
}

pub fn runtime_record_column_type(value: &Value) -> Option<crate::ast::ColumnType> {
    if matches!(value, Value::Null) {
        return None;
    }
    crate::ast::ColumnType::from_sql_name(crate::expr::value_type_name(value)).ok()
}

pub fn coerce_anonymous_record_value(
    context: &dyn RoutineValueContext,
    value: &Value,
    type_name: &str,
) -> Result<Value, SQLError> {
    let target = context
        .catalog_column_type(type_name)
        .or_else(|| crate::ast::ColumnType::from_sql_name(type_name).ok());
    let Some(target) = target else {
        return crate::assignment::routines::coerce_routine_value(context, value, type_name);
    };
    crate::assignment::conversion::convert_value_to_column_type_with_context(
        context,
        value.clone(),
        &target,
    )
    .map_err(|error| match error {
        SQLError::TypeMismatch(message) if message.starts_with("value too long for type ") => {
            SQLError::Routine {
                sqlstate: "22001".into(),
                message,
            }
        }
        other => other,
    })
}

pub fn anonymous_record_shape_error() -> SQLError {
    SQLError::Routine {
        sqlstate: "42P13".into(),
        message: "return type mismatch in function declared to return record".into(),
    }
}
pub fn call_output_schema(
    catalog: &dyn RoutineTypeCatalog,
    definition: &crate::ast::CreateFunction,
    parameter_types: &[String],
) -> Result<Option<crate::RowSchema>, SQLError> {
    let output_indices = definition
        .params
        .iter()
        .enumerate()
        .filter_map(|(index, parameter)| {
            matches!(
                parameter.mode,
                FunctionParamMode::Out | FunctionParamMode::InOut | FunctionParamMode::Table
            )
            .then_some(index)
        })
        .collect::<Vec<_>>();
    if output_indices.is_empty() {
        return Ok(None);
    }
    let columns = output_column_names(definition);
    let column_types = output_indices
        .into_iter()
        .map(|index| {
            catalog
                .resolve_catalog_column_type(&parameter_types[index])
                .or_else(|| crate::ast::ColumnType::from_sql_name(&parameter_types[index]).ok())
                .map(Some)
                .ok_or_else(|| {
                    SQLError::TypeMismatch(format!("unknown type `{}`", parameter_types[index]))
                })
        })
        .collect::<Result<Vec<_>, SQLError>>()?;
    Ok(Some(crate::RowSchema::with_types(columns, column_types)))
}
