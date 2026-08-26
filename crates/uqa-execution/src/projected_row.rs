//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Borrowed positional rows produced directly by projected storage reads.

use uqa_core::Value;
use uqa_sql::expr::RowLookup;

use crate::RowSchema;

/// Source of one logical value in a [`ProjectedRow`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectedValueSlot {
    /// Borrowed value at this position in the storage projection.
    Field(usize),
    /// Query-local value at this position in the extra-value slice.
    Extra(usize),
    /// A logical column that is absent from the projected source.
    Missing,
}

impl ProjectedValueSlot {
    /// Compile logical output columns against borrowed storage fields and query-local extras.
    #[must_use]
    pub fn compile(columns: &[String], fields: &[String], extras: &[&str]) -> Vec<Self> {
        columns
            .iter()
            .map(|column| {
                fields
                    .iter()
                    .position(|field| field == column)
                    .map(Self::Field)
                    .or_else(|| {
                        extras
                            .iter()
                            .position(|extra| *extra == column)
                            .map(Self::Extra)
                    })
                    .unwrap_or(Self::Missing)
            })
            .collect()
    }
}

/// Concrete borrowed row passed from a projected source into an aggregate executor.
///
/// Keeping this representation in the execution contract lets aggregate hot paths use positional values without a trait-object call for every group key and aggregate input. Extra values cover source metadata such as document identifiers and scores without materializing a named row.
pub struct ProjectedRow<'schema, 'row> {
    schema: &'schema RowSchema,
    slots: &'schema [ProjectedValueSlot],
    fields: &'row [&'row Value],
    extras: &'row [Option<Value>],
}

impl<'schema, 'row> ProjectedRow<'schema, 'row> {
    #[must_use]
    pub fn new(
        schema: &'schema RowSchema,
        slots: &'schema [ProjectedValueSlot],
        fields: &'row [&'row Value],
        extras: &'row [Option<Value>],
    ) -> Self {
        debug_assert_eq!(schema.len(), slots.len());
        Self {
            schema,
            slots,
            fields,
            extras,
        }
    }

    #[inline]
    #[must_use]
    pub fn positional_column(&self, index: usize) -> Option<&Value> {
        match self.slots.get(index)? {
            ProjectedValueSlot::Field(index) => self.fields.get(*index).copied(),
            ProjectedValueSlot::Extra(index) => self.extras.get(*index).and_then(Option::as_ref),
            ProjectedValueSlot::Missing => None,
        }
    }

    /// Materialize only at an operator boundary that requires owned physical values.
    #[must_use]
    pub fn into_values(self) -> Vec<Value> {
        (0..self.slots.len())
            .map(|index| {
                self.positional_column(index)
                    .cloned()
                    .unwrap_or(Value::Null)
            })
            .collect()
    }
}

impl RowLookup for ProjectedRow<'_, '_> {
    fn column(&self, name: &str) -> Option<&Value> {
        self.schema
            .unqualified_position(name)
            .and_then(|index| self.positional_column(index))
    }

    fn column_is_ambiguous(&self, name: &str) -> bool {
        self.schema.column_is_ambiguous(name)
    }

    fn qualified_column(&self, qualifier: &str, column: &str) -> Option<&Value> {
        self.schema
            .qualified_position(qualifier, column)
            .and_then(|index| self.positional_column(index))
    }

    fn qualified_column_is_ambiguous(&self, qualifier: &str, column: &str) -> bool {
        self.schema.qualified_column_is_ambiguous(qualifier, column)
    }

    #[inline]
    fn positional_column(&self, index: usize) -> Option<&Value> {
        Self::positional_column(self, index)
    }

    fn score_source(&self, qualifier: Option<&str>) -> Option<&Value> {
        let physical = self.schema.score_source_slot(qualifier)?;
        let logical = (0..self.schema.len())
            .find(|logical| self.schema.physical_slot(*logical) == Some(physical))?;
        self.positional_column(logical)
    }

    fn score_source_is_ambiguous(&self, qualifier: Option<&str>) -> bool {
        self.schema.score_source_is_ambiguous(qualifier)
    }

    fn visit_columns(&self, visitor: &mut dyn FnMut(&str, &Value)) {
        for (index, column) in self.schema.columns().iter().enumerate() {
            visitor(
                column,
                self.positional_column(index).unwrap_or(&Value::Null),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn projected_row_resolves_fields_extras_and_missing_slots() {
        let columns = vec!["name".into(), "score".into(), "missing".into()];
        let fields = vec!["name".into()];
        let slots = ProjectedValueSlot::compile(&columns, &fields, &["score"]);
        let schema = RowSchema::new(columns);
        let name = Value::Str("Ada".into());
        let values = [&name];
        let extras = [Some(Value::Float(0.75))];
        let row = ProjectedRow::new(&schema, &slots, &values, &extras);

        assert_eq!(row.positional_column(0), Some(&name));
        assert_eq!(row.positional_column(1), Some(&Value::Float(0.75)));
        assert_eq!(row.positional_column(2), None);
        assert_eq!(row.column("name"), Some(&name));
    }

    #[test]
    fn projected_row_preserves_structured_qualification() {
        let columns = vec!["id".into(), "id".into()];
        let schema = RowSchema::with_identities(
            columns.clone(),
            vec![
                crate::ColumnIdentity::qualified("left.dot", "id.dot"),
                crate::ColumnIdentity::qualified("right", "id.dot"),
            ],
            vec![None, None],
        );
        let slots = vec![ProjectedValueSlot::Field(0), ProjectedValueSlot::Field(1)];
        let left = Value::Int(1);
        let right = Value::Int(2);
        let values = [&left, &right];
        let row = ProjectedRow::new(&schema, &slots, &values, &[]);

        assert!(row.column_is_ambiguous("id.dot"));
        assert_eq!(
            row.qualified_column("left.dot", "id.dot"),
            Some(&Value::Int(1))
        );
        assert_eq!(row.qualified_column("left", "dot.id.dot"), None);
    }
}
