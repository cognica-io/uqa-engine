//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Qualified-column binding against structured physical schemas.

use uqa_sql::ast::ColumnType;
use uqa_sql::SQLError;

use crate::RowSchema;

pub(super) fn resolve(
    schema: &RowSchema,
    qualifier: &str,
    column: &str,
) -> Result<Option<ColumnType>, SQLError> {
    if schema.qualified_column_is_ambiguous(qualifier, column) {
        return Err(SQLError::AmbiguousColumn(format!("{qualifier}.{column}")));
    }
    if !schema.has_qualifier(qualifier) {
        return Err(SQLError::UnknownTable(qualifier.to_string()));
    }
    if !schema.has_qualified_column(qualifier, column) {
        return Err(SQLError::UnknownColumn(format!("{qualifier}.{column}")));
    }
    Ok(schema.qualified_type(qualifier, column).cloned())
}
