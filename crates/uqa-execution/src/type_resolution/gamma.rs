//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Compatibility API for `gamma(float8)` and `lgamma(float8)` overload resolution.

use uqa_sql::ast::{ColumnType, FunctionBinding};
use uqa_sql::SQLError;

use super::{
    fixed_builtin, function_resolution_error, FunctionTypeResolver, ResolvedFunctionOverload,
};

pub type ResolvedGammaOverload = ResolvedFunctionOverload;

#[doc(hidden)]
pub fn resolve_gamma_overload(
    name: &str,
    binding: Option<&FunctionBinding>,
    argument_names: &[Option<String>],
    argument_types: &[Option<ColumnType>],
    resolver: Option<&dyn FunctionTypeResolver>,
) -> Result<ResolvedGammaOverload, SQLError> {
    if !matches!(local_name(name).as_str(), "gamma" | "lgamma") {
        return Err(function_resolution_error(
            "42883",
            name,
            argument_names,
            argument_types,
            "does not exist",
        ));
    }
    fixed_builtin::resolve_overload(name, binding, argument_names, argument_types, resolver)
}

fn local_name(name: &str) -> String {
    let lower = name.to_ascii_lowercase();
    lower
        .strip_prefix("pg_catalog.")
        .unwrap_or(&lower)
        .to_string()
}
