//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Declaration collation selection and retained attribute identities.

use crate::{ast::ColumnDef, ColumnType, FunctionTypeResolver, SQLError};

/// `GetColumnDefCollation` resolves an explicit name before rejecting a noncollatable type. Expression COLLATE validation has a different diagnostic order.
pub fn select(
    types: &dyn FunctionTypeResolver,
    ty: &ColumnType,
    name: Option<&str>,
) -> Result<u32, SQLError> {
    let Some(name) = name else {
        return Ok(
            u32::try_from(crate::catalog::type_metadata::pg_type_collation_oid(ty))
                .expect("predefined collation identity"),
        );
    };
    let oid = super::expression::select(name, None, None, Some(types))?;
    super::expression::select(name, Some(oid), Some(ty), Some(types))
}

/// An unmodified legacy attribute retains the collation it actually used before explicit declarations were preserved.
#[must_use]
pub fn column_oid(column: &ColumnDef) -> i64 {
    column.collation_oid.map_or_else(
        || crate::catalog::type_metadata::pg_type_collation_oid(&column.ty),
        i64::from,
    )
}

#[cfg(test)]
mod tests;
