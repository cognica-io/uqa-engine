//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Compatibility API for JSON null-stripping overload resolution.

use uqa_sql::ast::{ColumnType, FunctionBinding};
use uqa_sql::SQLError;

use super::common::local_routine_name;
use super::{
    fixed_builtin, function_resolution_error, FunctionTypeResolver, ResolvedFunctionOverload,
};

pub type ResolvedJsonStripOverload = ResolvedFunctionOverload;

#[doc(hidden)]
pub fn resolve_json_strip_overload(
    name: &str,
    binding: Option<&FunctionBinding>,
    argument_names: &[Option<String>],
    argument_types: &[Option<ColumnType>],
    resolver: Option<&dyn FunctionTypeResolver>,
) -> Result<ResolvedJsonStripOverload, SQLError> {
    if !matches!(
        local_routine_name(name).as_str(),
        "json_strip_nulls" | "jsonb_strip_nulls"
    ) {
        return Err(function_resolution_error(
            "42883",
            name,
            argument_names,
            argument_types,
            "does not exist",
        ));
    }
    fixed_builtin::resolve_overload(
        name,
        binding,
        argument_names,
        argument_types,
        false,
        resolver,
    )
}
