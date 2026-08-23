//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! PostgreSQL-compatible binding for `crc32(bytea)` and `crc32c(bytea)`.

use super::string_binary::{self, Function, ResolvedStringBinaryOverload, CRC32, CRC32C};
use super::FunctionTypeResolver;
use crate::{RowSchema, ScalarExpr};
use uqa_sql::ast::{ColumnType, FunctionBinding};
use uqa_sql::{SQLError, SQLParam};

pub type ResolvedChecksumOverload = ResolvedStringBinaryOverload;

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
pub fn resolve_checksum_overload(
    name: &str,
    binding: Option<&FunctionBinding>,
    argument_names: &[Option<String>],
    argument_types: &[Option<ColumnType>],
    resolver: Option<&dyn FunctionTypeResolver>,
) -> Result<ResolvedChecksumOverload, SQLError> {
    let Some(function) = function(name) else {
        return Err(string_binary::undefined_function(
            name,
            argument_names,
            argument_types,
        ));
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
    [CRC32, CRC32C]
        .into_iter()
        .find(|function| function.matches(name))
}
