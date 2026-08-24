//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Compatibility adapters for string and binary built-in overload results.

use uqa_sql::ast::{ColumnType, FunctionBinding};
use uqa_sql::SQLError;

use super::{fixed_builtin, FunctionTypeResolver, ResolvedFunctionOverload};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedStringBinaryOverload {
    Builtin(ColumnType),
    User(ResolvedFunctionOverload),
}

#[doc(hidden)]
pub type ResolvedTextByteaOverload = ResolvedStringBinaryOverload;

pub(super) fn resolve_overload(
    name: &str,
    binding: Option<&FunctionBinding>,
    argument_names: &[Option<String>],
    argument_types: &[Option<ColumnType>],
    resolver: Option<&dyn FunctionTypeResolver>,
) -> Result<ResolvedStringBinaryOverload, SQLError> {
    let selected =
        fixed_builtin::resolve_overload(name, binding, argument_names, argument_types, resolver)?;
    if !selected.binding.builtin {
        return Ok(ResolvedStringBinaryOverload::User(selected));
    }
    let argument_type = selected
        .binding
        .argument_types
        .first()
        .ok_or_else(|| SQLError::Internal("fixed built-in binding lost its argument type".into()))
        .and_then(|ty| ColumnType::from_sql_name(ty))?;
    Ok(ResolvedStringBinaryOverload::Builtin(argument_type))
}
