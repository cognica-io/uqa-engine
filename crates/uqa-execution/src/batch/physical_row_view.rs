//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{PhysicalRow, ResultRow, RowLookup, RowSchema, Value, NULL_VALUE};

/// Stack-only schema/row pair implementing the scalar evaluator's read seam.
#[derive(Debug, Clone, Copy)]
pub struct PhysicalRowView<'a> {
    pub(super) schema: &'a RowSchema,
    pub(super) row: &'a PhysicalRow,
}

impl<'a> PhysicalRowView<'a> {
    pub fn get(&self, name: &str) -> Option<&'a Value> {
        self.schema
            .exact_slot(name)
            .and_then(|slot| self.row.value(slot))
    }

    pub fn value_at(&self, logical: usize) -> Option<&'a Value> {
        self.schema
            .slot(logical)
            .and_then(|slot| self.row.value(slot))
    }

    pub fn iter(&'a self) -> impl Iterator<Item = (&'a str, &'a Value)> + 'a {
        self.schema
            .columns()
            .iter()
            .enumerate()
            .map(|(logical, column)| {
                (
                    column.as_str(),
                    self.value_at(logical).unwrap_or(&NULL_VALUE),
                )
            })
    }

    pub fn to_result_row(&self) -> ResultRow {
        self.iter()
            .map(|(column, value)| (column.to_string(), value.clone()))
            .collect()
    }
}

impl RowLookup for PhysicalRowView<'_> {
    fn column(&self, name: &str) -> Option<&Value> {
        self.schema
            .column_slot(name)
            .and_then(|slot| self.row.value(slot))
    }

    fn column_is_ambiguous(&self, name: &str) -> bool {
        self.schema.column_is_ambiguous(name)
    }

    fn qualified_column(&self, qualifier: &str, column: &str) -> Option<&Value> {
        self.schema
            .qualified_slot(qualifier, column)
            .and_then(|slot| self.row.value(slot))
    }

    fn qualified_column_is_ambiguous(&self, qualifier: &str, column: &str) -> bool {
        self.schema.qualified_column_is_ambiguous(qualifier, column)
    }

    fn positional_column(&self, index: usize) -> Option<&Value> {
        self.value_at(index)
    }

    fn internal_column(&self, column: uqa_sql::ast::InternalColumnRef) -> Option<&Value> {
        self.schema
            .internal_slot(column)
            .and_then(|slot| self.row.value(slot))
    }

    fn score_source(&self, qualifier: Option<&str>) -> Option<&Value> {
        self.schema
            .score_source_slot(qualifier)
            .and_then(|slot| self.row.value(slot))
    }

    fn score_source_is_ambiguous(&self, qualifier: Option<&str>) -> bool {
        self.schema.score_source_is_ambiguous(qualifier)
    }

    fn visit_columns(&self, visitor: &mut dyn FnMut(&str, &Value)) {
        for (column, value) in self.iter() {
            visitor(column, value);
        }
    }
}
