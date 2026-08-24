//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Compatibility API for string and binary length overload resolution.

use uqa_sql::ast::{ColumnType, FunctionBinding};
use uqa_sql::SQLError;

use super::string_binary::{self, ResolvedStringBinaryOverload};
use super::{function_resolution_error, FunctionTypeResolver};

pub type ResolvedLengthOverload = ResolvedStringBinaryOverload;

#[doc(hidden)]
pub fn resolve_length_overload(
    name: &str,
    binding: Option<&FunctionBinding>,
    argument_names: &[Option<String>],
    argument_types: &[Option<ColumnType>],
    resolver: Option<&dyn FunctionTypeResolver>,
) -> Result<ResolvedLengthOverload, SQLError> {
    if !matches!(
        local_name(name).as_str(),
        "length" | "char_length" | "character_length" | "octet_length" | "bit_length"
    ) {
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

fn local_name(name: &str) -> String {
    let lower = name.to_ascii_lowercase();
    lower
        .strip_prefix("pg_catalog.")
        .unwrap_or(&lower)
        .to_string()
}
