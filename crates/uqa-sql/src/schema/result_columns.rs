//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Dense output descriptors without retained executor slots or source namespaces.

use super::RowSchema;

impl RowSchema {
    /// Select public output metadata in result order. Repeated labels retain their positional identities; a source with a different layout resolves a label through its existing schema index.
    pub fn for_result_columns(&self, columns: &[String]) -> Self {
        let positions = columns
            .iter()
            .enumerate()
            .map(|(position, column)| {
                if self.columns().get(position) == Some(column) {
                    Some(position)
                } else {
                    self.position(column)
                }
            })
            .collect::<Vec<_>>();
        Self::with_column_metadata(
            None,
            columns.iter().zip(&positions).map(|(column, position)| {
                (
                    column.clone(),
                    position
                        .and_then(|position| self.column_type(position))
                        .cloned(),
                    position.and_then(|position| self.column_collation(position)),
                )
            }),
        )
        .with_record_metadata(positions.into_iter().enumerate().filter_map(
            |(output, input)| {
                self.record_metadata(input?)
                    .cloned()
                    .map(|metadata| (output, metadata))
            },
        ))
    }
}
