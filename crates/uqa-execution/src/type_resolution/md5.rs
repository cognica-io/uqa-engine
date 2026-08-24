//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Compatibility API for `md5(text|bytea)` overload resolution.

use uqa_sql::ast::{ColumnType, FunctionBinding};
use uqa_sql::SQLError;

use super::common::local_routine_name;
use super::string_binary::{self, ResolvedStringBinaryOverload};
use super::{function_resolution_error, FunctionTypeResolver};

pub type ResolvedMd5Overload = ResolvedStringBinaryOverload;

#[doc(hidden)]
pub fn resolve_md5_overload(
    name: &str,
    binding: Option<&FunctionBinding>,
    argument_names: &[Option<String>],
    argument_types: &[Option<ColumnType>],
    resolver: Option<&dyn FunctionTypeResolver>,
) -> Result<ResolvedMd5Overload, SQLError> {
    if local_routine_name(name) != "md5" {
        return Err(function_resolution_error(
            "42883",
            name,
            argument_names,
            argument_types,
            "does not exist",
        ));
    }
    string_binary::resolve_overload(name, binding, argument_names, argument_types, resolver)
}
