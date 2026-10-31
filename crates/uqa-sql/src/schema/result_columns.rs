//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Dense output descriptors without retained executor slots or source namespaces.

use super::RowSchema;
use crate::SQLError;
use uqa_core::memory::ProductionControl;

impl RowSchema {
    /// Select public output metadata in result order. Repeated labels retain their positional identities; a source with a different layout resolves a label through its existing schema index.
    pub fn for_result_columns(&self, columns: &[String]) -> Self {
        self.for_result_columns_with_control(columns, &ProductionControl::uncontrolled())
            .expect("ordinary result descriptor")
    }

    /// Publish dense public metadata while retaining selected record descriptors and admitting all new column, identity and type storage before construction.
    pub fn for_result_columns_with_control(
        &self,
        columns: &[String],
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        Self::with_output_metadata_with_control(
            None,
            columns.iter().enumerate().map(|(position, column)| {
                let position = if self.columns().get(position) == Some(column) {
                    Some(position)
                } else {
                    self.position(column)
                };
                (
                    column.as_str(),
                    position.and_then(|position| self.column_type(position)),
                    position.and_then(|position| self.column_collation(position)),
                    position
                        .and_then(|position| self.record_metadata(position))
                        .cloned(),
                )
            }),
            false,
            control,
        )
    }

    /// Project explicitly selected source positions into a dense public result. This combines logical-position remapping and result publication without constructing temporary source names or hidden namespace indexes.
    pub fn for_selected_result_columns_with_control<'a>(
        &self,
        columns: impl IntoIterator<Item = (&'a str, usize)>,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        Self::with_output_metadata_with_control(
            None,
            columns.into_iter().map(|(name, position)| {
                (
                    name,
                    self.column_type(position),
                    self.inherited_column_collation(position),
                    self.record_metadata(position).cloned(),
                )
            }),
            false,
            control,
        )
    }
}
