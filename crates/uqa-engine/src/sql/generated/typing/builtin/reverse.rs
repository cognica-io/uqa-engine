//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Generated-column typing for `PostgreSQL` `reverse(text|bytea)`.

use crate::engine_user_functions::routine_signature_types;
use crate::sql::{ColumnType, Engine, SQLError, Value};
use uqa_sql::ast::{ColumnDef, Expr, FunctionBinding, GeneratedFunctionDependency};

use super::super::{
    generation_type_name, named_argument, non_immutable_function, validate_unknown_literal_cast,
    GenerationType,
};

pub(in super::super) struct ReverseCall<'a> {
    pub(in super::super) engine: &'a Engine,
    pub(in super::super) columns: &'a [ColumnDef],
    pub(in super::super) name: &'a str,
    pub(in super::super) args: &'a [Expr],
    pub(in super::super) argument_names: &'a [Option<String>],
    pub(in super::super) argument_types: &'a [GenerationType],
}

pub(in super::super) fn bind_call(
    call: ReverseCall<'_>,
    binding: &mut Option<FunctionBinding>,
    dependencies: &mut Vec<GeneratedFunctionDependency>,
) -> Result<bool, SQLError> {
    if !is_function(call.name) {
        return Ok(false);
    }
    let declared_argument_types = call
        .args
        .iter()
        .zip(call.argument_types)
        .map(|(argument, inferred)| {
            let (_, value) = named_argument(argument)?;
            Ok(generation_expression_column_type(
                call.columns,
                value,
                inferred,
            ))
        })
        .collect::<Result<Vec<_>, SQLError>>()?;
    match uqa_execution::resolve_reverse_overload(
        call.name,
        binding.as_ref(),
        call.argument_names,
        &declared_argument_types,
        Some(call.engine),
    )? {
        uqa_execution::ResolvedReverseOverload::Builtin(argument_type) => {
            *binding = Some(FunctionBinding {
                name: "pg_catalog.reverse".into(),
                argument_types: vec![argument_type.regtype_name()],
                builtin: true,
            });
        }
        uqa_execution::ResolvedReverseOverload::User(overload) => {
            let selected = validate_bound_function(
                call.engine,
                &overload.binding,
                call.argument_names,
                call.argument_types,
            )?;
            dependencies.push(selected.clone());
            *binding = Some(selected);
        }
    }
    Ok(true)
}

pub(super) fn require_signature(
    name: &str,
    argument_names: &[Option<String>],
    args: &[GenerationType],
) -> Result<GenerationType, SQLError> {
    if argument_names != [None] {
        return Err(undefined_function(name, argument_names, args));
    }
    match args {
        [GenerationType::Bytea] => Ok(GenerationType::Bytea),
        [GenerationType::Text | GenerationType::Null | GenerationType::UnknownLiteral(_)] => {
            Ok(GenerationType::Text)
        }
        _ => Err(undefined_function(name, argument_names, args)),
    }
}

fn undefined_function(
    name: &str,
    argument_names: &[Option<String>],
    args: &[GenerationType],
) -> SQLError {
    let signature = args
        .iter()
        .zip(argument_names)
        .map(|(argument, argument_name)| {
            let argument = generation_type_name(argument);
            argument_name
                .as_ref()
                .map_or(argument.clone(), |name| format!("{name} => {argument}"))
        })
        .collect::<Vec<_>>()
        .join(", ");
    SQLError::Routine {
        sqlstate: "42883".into(),
        message: format!("function {name}({signature}) does not exist"),
    }
}

fn is_function(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.strip_prefix("pg_catalog.").unwrap_or(&lower) == "reverse"
}

fn generation_expression_column_type(
    columns: &[ColumnDef],
    expression: &Expr,
    inferred: &GenerationType,
) -> Option<ColumnType> {
    match expression {
        Expr::Column(name) | Expr::QualifiedColumn { column: name, .. } => columns
            .iter()
            .find(|column| column.name == *name)
            .map(|column| column.ty.clone()),
        Expr::Cast { ty, .. } => ColumnType::from_sql_name(ty).ok(),
        Expr::Literal(Value::Str(_) | Value::Null) => None,
        _ => ColumnType::from_sql_name(&generation_type_name(inferred)).ok(),
    }
}

fn validate_bound_function(
    engine: &Engine,
    binding: &FunctionBinding,
    argument_names: &[Option<String>],
    argument_types: &[GenerationType],
) -> Result<FunctionBinding, SQLError> {
    let function = engine
        .lookup_sql_functions(&binding.name)
        .and_then(|overloads| {
            overloads
                .into_iter()
                .find(|function| routine_signature_types(&function.def) == binding.argument_types)
        })
        .ok_or_else(|| SQLError::UnknownFunction(binding.name.clone()))?;
    if function.def.is_procedure || function.def.returns_set() {
        return Err(SQLError::TypeMismatch(format!(
            "generated-column function `{}` must return one scalar value",
            binding.name
        )));
    }
    if function.def.volatility != uqa_sql::ast::FunctionVolatility::Immutable {
        return Err(non_immutable_function(&binding.name));
    }
    let signature = function.def.signature_params();
    let mut positional = 0usize;
    for (argument_name, argument_type) in argument_names.iter().zip(argument_types) {
        let position = argument_name.as_ref().map_or_else(
            || {
                let position = positional;
                positional += 1;
                position
            },
            |argument_name| {
                signature
                    .iter()
                    .position(|parameter| parameter.name == *argument_name)
                    .unwrap_or(signature.len())
            },
        );
        let parameter = signature.get(position).ok_or_else(|| {
            SQLError::Internal(format!(
                "resolved generated-column function `{}` lost its argument mapping",
                binding.name
            ))
        })?;
        validate_unknown_literal_cast(argument_type, &parameter.type_name)?;
    }
    Ok(binding.clone())
}
