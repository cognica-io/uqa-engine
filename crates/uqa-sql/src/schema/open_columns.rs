//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Deferred row descriptors for document and native-function sources.

use super::RowSchema;

impl RowSchema {
    /// Mark a source whose remaining column names are supplied at execution. This is analysis metadata: it allocates no physical columns and never changes runtime lookup or wildcard expansion.
    pub fn with_open_columns(input: &Self, qualifier: Option<&str>) -> Self {
        let mut result = input.clone();
        if !input
            .index
            .cold
            .open_qualifiers
            .iter()
            .any(|name| name.as_deref() == qualifier)
        {
            std::sync::Arc::make_mut(&mut result.index)
                .cold
                .open_qualifiers
                .insert(qualifier.map(Box::<str>::from));
        }
        result
    }

    /// Whether unresolved names in this namespace require the source's runtime descriptor. An unqualified lookup can depend on any open source.
    #[must_use]
    pub fn columns_are_open(&self, qualifier: Option<&str>) -> bool {
        qualifier.map_or_else(
            || !self.index.cold.open_qualifiers.is_empty(),
            |qualifier| {
                self.index
                    .cold
                    .open_qualifiers
                    .iter()
                    .any(|candidate| candidate.as_deref() == Some(qualifier))
            },
        )
    }
}
