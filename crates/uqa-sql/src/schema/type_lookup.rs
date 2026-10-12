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
    /// Reuse an already analyzed expression occurrence in this scope. `Some(None)` preserves an observed unknown type; `None` asks the caller to infer the expression normally. Borrowed observations must not outlive their original expressions or parameter environment.
    fn observed_expression_type(&self, _: &crate::ScalarExpr) -> Option<Option<&ColumnType>> {
        None
    }

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

    fn column_collation(&self, position: usize) -> Option<u32> {
        self.column_type(position).map(default_collation)
    }
    fn collation_of(&self, name: &str) -> Option<u32> {
        self.type_of(name).map(default_collation)
    }
    fn qualified_collation(&self, qualifier: &str, name: &str) -> Option<u32> {
        self.qualified_type(qualifier, name).map(default_collation)
    }
    fn internal_collation(&self, column: InternalColumnRef) -> Option<u32> {
        self.internal_type(column).map(default_collation)
    }

    /// Preserve the physical outer-row interface used by existing custom scalar-subquery resolvers. Declared column views have no query arena or physical outer row.
    fn physical_schema(&self) -> Option<&RowSchema>;
}

fn default_collation(ty: &ColumnType) -> u32 {
    u32::try_from(crate::catalog::type_metadata::pg_type_collation_oid(ty))
        .expect("type collation identity")
}

impl ScalarTypeSchema for RowSchema {
    fn column_collation(&self, position: usize) -> Option<u32> {
        Self::column_collation(self, position)
    }
    fn collation_of(&self, name: &str) -> Option<u32> {
        Self::collation_of(self, name)
    }
    fn qualified_collation(&self, qualifier: &str, name: &str) -> Option<u32> {
        Self::qualified_collation(self, qualifier, name)
    }
    fn internal_collation(&self, column: InternalColumnRef) -> Option<u32> {
        Self::internal_collation(self, column)
    }

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

    /// Original declarations, including stable attribute numbers and selected collations, borrowed from the same catalog generation as type lookup.
    pub fn columns(&self) -> &'a [ColumnDef] {
        self.columns
    }
}

impl ScalarTypeSchema for ColumnTypeSchema<'_> {
    fn column_collation(&self, position: usize) -> Option<u32> {
        self.columns.get(position).map(|column| {
            column
                .collation_oid
                .unwrap_or_else(|| default_collation(&column.ty))
        })
    }
    fn collation_of(&self, name: &str) -> Option<u32> {
        let mut matching = self
            .columns
            .iter()
            .enumerate()
            .filter(|(_, column)| column.name == name);
        let (position, _) = matching.next()?;
        matching
            .next()
            .is_none()
            .then(|| self.column_collation(position))
            .flatten()
    }

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
