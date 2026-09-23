//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Qualified-column binding against structured physical schemas.

use crate::ast::ColumnType;
use crate::SQLError;

use crate::schema::ScalarTypeSchema;

pub(super) fn resolve_with_control(
    schema: &dyn ScalarTypeSchema,
    qualifier: &str,
    column: &str,
    control: &uqa_core::memory::ProductionControl<'_>,
) -> Result<Option<uqa_core::memory::Produced<ColumnType>>, SQLError> {
    control.check()?;
    if schema.qualified_column_is_ambiguous(qualifier, column) {
        return Err(SQLError::AmbiguousColumn(format!("{qualifier}.{column}")));
    }
    if !schema.has_qualifier(qualifier) {
        return Err(SQLError::UnknownTable(qualifier.to_string()));
    }
    if !schema.has_qualified_column(qualifier, column) && !schema.columns_are_open(Some(qualifier))
    {
        return Err(SQLError::unknown_qualified_column(qualifier, column));
    }
    schema
        .qualified_type(qualifier, column)
        .map(|ty| ty.clone_with_control(control).map_err(Into::into))
        .transpose()
}
