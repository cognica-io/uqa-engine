//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! PostgreSQL-compatible binding for `md5(text|bytea)`.

use super::string_binary::{self, ResolvedStringBinaryOverload, MD5};
use super::FunctionTypeResolver;
use crate::{RowSchema, ScalarExpr};
use uqa_sql::ast::{ColumnType, FunctionBinding};
use uqa_sql::{SQLError, SQLParam};

pub type ResolvedMd5Overload = ResolvedStringBinaryOverload;

pub(super) fn resolve_type(
    name: &str,
    binding: Option<&FunctionBinding>,
    args: &[ScalarExpr],
    argument_types: &[Option<ColumnType>],
    resolver: Option<&dyn FunctionTypeResolver>,
) -> Result<Option<ColumnType>, SQLError> {
    string_binary::resolve_type(MD5, name, binding, args, argument_types, resolver)
}

#[doc(hidden)]
pub fn resolve_md5_overload(
    name: &str,
    binding: Option<&FunctionBinding>,
    argument_names: &[Option<String>],
    argument_types: &[Option<ColumnType>],
    resolver: Option<&dyn FunctionTypeResolver>,
) -> Result<ResolvedMd5Overload, SQLError> {
    string_binary::resolve_overload(MD5, name, binding, argument_names, argument_types, resolver)
}

pub(super) fn builtin_argument_type(argument_types: &[Option<ColumnType>]) -> Option<ColumnType> {
    string_binary::builtin_argument_type(MD5, argument_types)
}

pub(super) fn is_function(name: &str) -> bool {
    MD5.matches(name)
}

pub(super) fn bind_call(
    name: String,
    binding: &mut Option<FunctionBinding>,
    args: &mut [ScalarExpr],
    schema: &RowSchema,
    params: &[SQLParam],
    resolver: Option<&dyn FunctionTypeResolver>,
) -> String {
    string_binary::bind_call(MD5, name, binding, args, schema, params, resolver)
}
