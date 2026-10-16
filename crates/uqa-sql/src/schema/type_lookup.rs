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
    /// The named composite identity of a relation's whole row, when the caller retains it. Query-only scopes may expose an anonymous record instead.
    fn whole_row_type(&self, _qualifier: &str) -> Option<&ColumnType> {
        None
    }
    fn has_qualified_column(&self, qualifier: &str, name: &str) -> bool;
    fn qualified_column_is_ambiguous(&self, qualifier: &str, name: &str) -> bool;
    fn qualified_type(&self, qualifier: &str, name: &str) -> Option<&ColumnType>;
    fn columns_are_open(&self, qualifier: Option<&str>) -> bool;

    /// Observe the analyzed result selection or a direct input reference without repeating type input, evaluation or collation derivation.
    fn input_collation(&self, expression: &crate::ScalarExpr) -> Option<u32> {
        match expression {
            crate::ScalarExpr::TypedLiteral { collation_oid, .. } => *collation_oid,
            crate::ScalarExpr::Collated { collation, .. } => Some(collation.oid()),
            crate::ScalarExpr::Func {
                binding: Some(binding),
                ..
            } => match binding.dispatch {
                Some(crate::ast::FunctionDispatch::Collate { oid }) => oid,
                _ => None,
            },
            crate::ScalarExpr::Column(name) => self.collation_of(name),
            crate::ScalarExpr::QualifiedColumn { qualifier, column } => {
                self.qualified_collation(qualifier, column)
            }
            crate::ScalarExpr::Position(position) => self.column_collation(*position),
            crate::ScalarExpr::InternalColumn(column) => self.internal_collation(*column),
            _ => None,
        }
    }

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
    fn whole_row_type(&self, qualifier: &str) -> Option<&ColumnType> {
        self.index.cold.row_types.get(qualifier)
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
    relation: Option<(&'a str, &'a ColumnType)>,
}

impl<'a> ColumnTypeSchema<'a> {
    pub fn new(columns: &'a [ColumnDef]) -> Self {
        Self {
            columns,
            relation: None,
        }
    }

    /// Retain the declaring relation's actual row type without copying its columns or representing system attributes as user columns.
    pub fn with_relation(
        columns: &'a [ColumnDef],
        name: &'a str,
        row_type: &'a ColumnType,
    ) -> Result<Self, crate::SQLError> {
        if !matches!(row_type, ColumnType::Composite(_)) {
            return Err(crate::SQLError::Internal(
                "a relation scope requires its named composite row type".into(),
            ));
        }
        Ok(Self {
            columns,
            relation: Some((name, row_type)),
        })
    }

    pub(crate) fn system_column_type(&self, name: &str) -> Option<&ColumnType> {
        (self.relation.is_some() && name == "tableoid").then_some(&ColumnType::Oid)
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
        let Some((position, _)) = matching.next() else {
            return self.system_column_type(name).map(default_collation);
        };
        matching
            .next()
            .is_none()
            .then(|| self.column_collation(position))
            .flatten()
    }

    fn has_unqualified_column(&self, name: &str) -> bool {
        self.columns.iter().any(|column| column.name == name)
            || self.system_column_type(name).is_some()
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
        let Some(first) = matching.next() else {
            return self.system_column_type(name);
        };
        matching.next().is_none().then_some(&first.ty)
    }
    fn column_type(&self, position: usize) -> Option<&ColumnType> {
        self.columns.get(position).map(|column| &column.ty)
    }
    fn internal_type(&self, _: InternalColumnRef) -> Option<&ColumnType> {
        None
    }
    fn has_qualifier(&self, qualifier: &str) -> bool {
        self.whole_row_type(qualifier).is_some()
    }
    fn whole_row_type(&self, qualifier: &str) -> Option<&ColumnType> {
        let (name, row_type) = self.relation?;
        let ColumnType::Composite(reference) = row_type else {
            unreachable!("relation scope validates its row type");
        };
        (qualifier == name || qualifier == reference.name).then_some(row_type)
    }
    fn has_qualified_column(&self, qualifier: &str, name: &str) -> bool {
        self.has_qualifier(qualifier) && self.has_unqualified_column(name)
    }
    fn qualified_column_is_ambiguous(&self, qualifier: &str, name: &str) -> bool {
        self.has_qualifier(qualifier) && self.column_is_ambiguous(name)
    }
    fn qualified_type(&self, qualifier: &str, name: &str) -> Option<&ColumnType> {
        self.has_qualifier(qualifier)
            .then(|| self.type_of(name))
            .flatten()
    }
    fn qualified_collation(&self, qualifier: &str, name: &str) -> Option<u32> {
        self.has_qualifier(qualifier)
            .then(|| self.collation_of(name))
            .flatten()
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
