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
    match ty {
        ColumnType::Named(name) => resolver
            .resolve_type_name(name)?
            .map_or_else(|| ColumnType::from_sql_name(name), Ok),
        ColumnType::Array(element) => resolve_declared_column_type(resolver, element)
            .map(|element| ColumnType::Array(Box::new(element))),
        other => Ok(other.clone()),
    }
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
