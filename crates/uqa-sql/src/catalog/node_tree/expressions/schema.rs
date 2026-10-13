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

    /// Whole-row Vars retain attribute zero and the relation's actual named type, independently of its current live attribute list.
    fn whole_row_reference(&self, qualifier: &str) -> Result<ColumnReference<'_>, SQLError> {
        self.whole_row_type(qualifier)
            .map(|ty| ColumnReference {
                attribute_number: 0,
                ty,
                collation_oid: 0,
            })
            .ok_or_else(|| SQLError::UnknownTable(qualifier.into()))
    }
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
        if qualifier.is_some_and(|qualifier| !self.has_qualifier(qualifier)) {
            return Err(SQLError::UnknownColumn(name.into()));
        }
        let mut matching = self
            .columns()
            .iter()
            .enumerate()
            .filter(|(_, column)| column.name == name);
        let Some((position, column)) = matching.next() else {
            if let Some(ty) = self.system_column_type(name) {
                return Ok(ColumnReference {
                    attribute_number: -6,
                    ty,
                    collation_oid: 0,
                });
            }
            if qualifier.is_none() && self.has_qualifier(name) {
                return self.whole_row_reference(name);
            }
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
