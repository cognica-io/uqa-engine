//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Scalar type lookup borrows declared columns without constructing physical row indexes.

use super::RowSchema;
use crate::ast::{ColumnDef, ColumnType, InternalColumnRef};

/// Borrowed schema operations used by scalar type propagation and binding. Physical schemas retain their complete alias, ambiguity, open-column and executor-attribute behavior; declared column slices supply an unqualified positional namespace without allocating another schema generation.
pub trait ScalarTypeSchema {
    fn has_unqualified_column(&self, name: &str) -> bool;
    fn column_is_ambiguous(&self, name: &str) -> bool;
    fn type_of(&self, name: &str) -> Option<&ColumnType>;
    fn column_type(&self, position: usize) -> Option<&ColumnType>;
    fn internal_type(&self, column: InternalColumnRef) -> Option<&ColumnType>;
    fn has_qualifier(&self, qualifier: &str) -> bool;
    fn has_qualified_column(&self, qualifier: &str, name: &str) -> bool;
    fn qualified_column_is_ambiguous(&self, qualifier: &str, name: &str) -> bool;
    fn qualified_type(&self, qualifier: &str, name: &str) -> Option<&ColumnType>;
    fn columns_are_open(&self, qualifier: Option<&str>) -> bool;

    /// Preserve the physical outer-row interface used by existing custom scalar-subquery resolvers. Declared column views have no query arena or physical outer row.
    fn physical_schema(&self) -> Option<&RowSchema>;
}

impl ScalarTypeSchema for RowSchema {
    fn has_unqualified_column(&self, name: &str) -> bool {
        Self::has_unqualified_column(self, name)
    }
    fn column_is_ambiguous(&self, name: &str) -> bool {
        Self::column_is_ambiguous(self, name)
    }
    fn type_of(&self, name: &str) -> Option<&ColumnType> {
        Self::type_of(self, name)
    }
    fn column_type(&self, position: usize) -> Option<&ColumnType> {
        Self::column_type(self, position)
    }
    fn internal_type(&self, column: InternalColumnRef) -> Option<&ColumnType> {
        Self::internal_type(self, column)
    }
    fn has_qualifier(&self, qualifier: &str) -> bool {
        Self::has_qualifier(self, qualifier)
    }
    fn has_qualified_column(&self, qualifier: &str, name: &str) -> bool {
        Self::has_qualified_column(self, qualifier, name)
    }
    fn qualified_column_is_ambiguous(&self, qualifier: &str, name: &str) -> bool {
        Self::qualified_column_is_ambiguous(self, qualifier, name)
    }
    fn qualified_type(&self, qualifier: &str, name: &str) -> Option<&ColumnType> {
        Self::qualified_type(self, qualifier, name)
    }
    fn columns_are_open(&self, qualifier: Option<&str>) -> bool {
        Self::columns_are_open(self, qualifier)
    }
    fn physical_schema(&self) -> Option<&RowSchema> {
        Some(self)
    }
}

/// Zero-allocation view matching `RowSchema::with_types` for the supplied unqualified column definitions. Names and declared types, including domains and nested arrays, stay borrowed from their existing owner. Duplicate exact names remain ambiguous; dots in an identifier do not create a relation qualifier.
#[derive(Clone, Copy)]
pub struct ColumnTypeSchema<'a> {
    columns: &'a [ColumnDef],
}

impl<'a> ColumnTypeSchema<'a> {
    pub fn new(columns: &'a [ColumnDef]) -> Self {
        Self { columns }
    }
}

impl ScalarTypeSchema for ColumnTypeSchema<'_> {
    fn has_unqualified_column(&self, name: &str) -> bool {
        self.columns.iter().any(|column| column.name == name)
    }
    fn column_is_ambiguous(&self, name: &str) -> bool {
        self.columns
            .iter()
            .filter(|column| column.name == name)
            .nth(1)
            .is_some()
    }
    fn type_of(&self, name: &str) -> Option<&ColumnType> {
        let mut matching = self.columns.iter().filter(|column| column.name == name);
        let first = matching.next()?;
        matching.next().is_none().then_some(&first.ty)
    }
    fn column_type(&self, position: usize) -> Option<&ColumnType> {
        self.columns.get(position).map(|column| &column.ty)
    }
    fn internal_type(&self, _: InternalColumnRef) -> Option<&ColumnType> {
        None
    }
    fn has_qualifier(&self, _: &str) -> bool {
        false
    }
    fn has_qualified_column(&self, _: &str, _: &str) -> bool {
        false
    }
    fn qualified_column_is_ambiguous(&self, _: &str, _: &str) -> bool {
        false
    }
    fn qualified_type(&self, _: &str, _: &str) -> Option<&ColumnType> {
        None
    }
    fn columns_are_open(&self, _: Option<&str>) -> bool {
        false
    }
    fn physical_schema(&self) -> Option<&RowSchema> {
        None
    }
}

#[cfg(test)]
mod tests;
