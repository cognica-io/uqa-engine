//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Borrowed catalog attributes used by stored expression encoding.

use crate::schema::{ColumnTypeSchema, ScalarTypeSchema};
use crate::{ColumnType, RowSchema, SQLError};

/// A resolved attribute in the same generation as the expression's type lookup. Attribute numbers are catalog identities, not positions after projection or a dropped column.
pub struct ColumnReference<'a> {
    pub attribute_number: i64,
    pub ty: &'a ColumnType,
    pub collation_oid: i64,
}

/// Stored expression encoding needs the original attribute identity and collation as well as its type. Borrow declarations directly when those identities are available; an execution-only schema retains its existing positional contract.
pub trait ExpressionSchema: ScalarTypeSchema {
    fn column_reference(
        &self,
        name: &str,
        qualifier: Option<&str>,
    ) -> Result<ColumnReference<'_>, SQLError>;
}

impl ExpressionSchema for RowSchema {
    fn column_reference(
        &self,
        name: &str,
        qualifier: Option<&str>,
    ) -> Result<ColumnReference<'_>, SQLError> {
        let position = qualifier
            .map_or_else(
                || self.unqualified_position(name),
                |qualifier| self.qualified_position(qualifier, name),
            )
            .ok_or_else(|| SQLError::UnknownColumn(name.into()))?;
        let ty = self
            .column_type(position)
            .ok_or_else(|| SQLError::Internal("catalog column has no declared type".into()))?;
        Ok(ColumnReference {
            attribute_number: i64::try_from(position + 1)
                .map_err(|_| SQLError::Internal("column ordinal overflow".into()))?,
            ty,
            collation_oid: i64::from(
                self.column_collation(position)
                    .expect("typed catalog column collation"),
            ),
        })
    }
}

impl ExpressionSchema for ColumnTypeSchema<'_> {
    fn column_reference(
        &self,
        name: &str,
        qualifier: Option<&str>,
    ) -> Result<ColumnReference<'_>, SQLError> {
        let mut matching = self
            .columns()
            .iter()
            .enumerate()
            .filter(|(_, column)| qualifier.is_none() && column.name == name);
        let Some((position, column)) = matching.next() else {
            return Err(SQLError::UnknownColumn(name.into()));
        };
        if matching.next().is_some() {
            return Err(SQLError::UnknownColumn(name.into()));
        }
        let attribute_number = column.attribute_number.map_or_else(
            || {
                i64::try_from(position + 1)
                    .map_err(|_| SQLError::Internal("column ordinal overflow".into()))
            },
            |number| Ok(i64::from(number)),
        )?;
        if attribute_number <= 0 {
            return Err(SQLError::Internal(
                "invalid catalog attribute number".into(),
            ));
        }
        Ok(ColumnReference {
            attribute_number,
            ty: &column.ty,
            collation_oid: crate::catalog::collations::declarations::column_oid(column),
        })
    }
}
