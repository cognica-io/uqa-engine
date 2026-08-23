//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! PostgreSQL-compatible binding for one-argument string and binary length functions.

use super::string_binary::{
    self, Function, ResolvedStringBinaryOverload, BIT_LENGTH, CHARACTER_LENGTH, CHAR_LENGTH,
    LENGTH, OCTET_LENGTH,
};
use super::FunctionTypeResolver;
use crate::{RowSchema, ScalarExpr};
use uqa_sql::ast::{ColumnType, FunctionBinding};
use uqa_sql::{SQLError, SQLParam};

pub type ResolvedLengthOverload = ResolvedStringBinaryOverload;

pub(super) fn resolve_type(
    name: &str,
    binding: Option<&FunctionBinding>,
    args: &[ScalarExpr],
    argument_types: &[Option<ColumnType>],
    resolver: Option<&dyn FunctionTypeResolver>,
) -> Result<Option<ColumnType>, SQLError> {
    let Some(function) = function(name) else {
        return Ok(None);
    };
    string_binary::resolve_type(function, name, binding, args, argument_types, resolver)
}

#[doc(hidden)]
pub fn resolve_length_overload(
    name: &str,
    binding: Option<&FunctionBinding>,
    argument_names: &[Option<String>],
    argument_types: &[Option<ColumnType>],
    resolver: Option<&dyn FunctionTypeResolver>,
) -> Result<ResolvedLengthOverload, SQLError> {
    let Some(function) = function(name) else {
        return Err(undefined_function(name, argument_names, argument_types));
    };
    string_binary::resolve_overload(
        function,
        name,
        binding,
        argument_names,
        argument_types,
        resolver,
    )
}

pub(super) fn builtin_argument_type(
    name: &str,
    argument_types: &[Option<ColumnType>],
) -> Option<ColumnType> {
    function(name)
        .and_then(|function| string_binary::builtin_argument_type(function, argument_types))
}

pub(super) fn is_function(name: &str) -> bool {
    function(name).is_some()
}

pub(super) fn bind_call(
    name: String,
    binding: &mut Option<FunctionBinding>,
    args: &mut [ScalarExpr],
    schema: &RowSchema,
    params: &[SQLParam],
    resolver: Option<&dyn FunctionTypeResolver>,
) -> String {
    let Some(function) = function(&name) else {
        return name;
    };
    string_binary::bind_call(function, name, binding, args, schema, params, resolver)
}

fn function(name: &str) -> Option<Function> {
    let lower = name.to_ascii_lowercase();
    match lower.strip_prefix("pg_catalog.").unwrap_or(&lower) {
        "length" => Some(LENGTH),
        "char_length" => Some(CHAR_LENGTH),
        "character_length" => Some(CHARACTER_LENGTH),
        "octet_length" => Some(OCTET_LENGTH),
        "bit_length" => Some(BIT_LENGTH),
        _ => None,
    }
}

fn undefined_function(
    name: &str,
    argument_names: &[Option<String>],
    argument_types: &[Option<ColumnType>],
) -> SQLError {
    let signature = argument_names
        .iter()
        .zip(argument_types)
        .map(|(argument_name, argument_type)| {
            let argument_type = argument_type
                .as_ref()
                .map_or_else(|| "unknown".into(), ColumnType::regtype_name);
            argument_name
                .as_ref()
                .map_or(argument_type.clone(), |argument_name| {
                    format!("{argument_name} => {argument_type}")
                })
        })
        .collect::<Vec<_>>()
        .join(", ");
    SQLError::Routine {
        sqlstate: "42883".into(),
        message: format!("function {name}({signature}) does not exist"),
    }
}
