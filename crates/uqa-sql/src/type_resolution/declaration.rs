//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Resolve declared column types through catalog type metadata.

use super::FunctionTypeResolver;
use crate::{ColumnType, SQLError};

pub fn resolve_declared_column_type(
    resolver: &dyn FunctionTypeResolver,
    ty: &ColumnType,
) -> Result<ColumnType, SQLError> {
    let mut element = ty;
    let mut dimensions = 0;
    while let ColumnType::Array(inner) = element {
        element = inner;
        dimensions += 1;
    }
    // A shell has no array OID: resolve the complete name before its element.
    if let ColumnType::Named(name) = element {
        let name = if dimensions == 0 {
            std::borrow::Cow::Borrowed(name.as_str())
        } else {
            std::borrow::Cow::Owned(format!("{name}{}", "[]".repeat(dimensions)))
        };
        return resolver
            .resolve_type_name(&name)?
            .map_or_else(|| ColumnType::from_sql_name(&name), Ok);
    }
    Ok(ty.clone())
}

/// Resolve an ALTER declaration after its relation and column checks. The compiler keeps modifier syntax intact until this boundary, including declarations an `IF NOT EXISTS` action can skip.
pub fn resolve_alter_column_type(
    resolver: &dyn FunctionTypeResolver,
    ty: &ColumnType,
) -> Result<ColumnType, SQLError> {
    match ty {
        ColumnType::Named(name) => {
            let ty = crate::compiler::compile_retained_type_declaration(name)?;
            resolve_declared_column_type(resolver, &ty)
        }
        ColumnType::Array(element) => resolve_alter_column_type(resolver, element)
            .map(|element| ColumnType::Array(Box::new(element))),
        other => Ok(other.clone()),
    }
}
