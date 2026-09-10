//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Deferred row descriptors for document and native-function sources.

use super::{RowSchema, SchemaBuildMetadata};

impl RowSchema {
    /// Mark a source whose remaining column names are supplied at execution. This is analysis metadata: it allocates no physical columns and never changes runtime lookup or wildcard expansion.
    pub fn with_open_columns(input: &Self, qualifier: Option<&str>) -> Self {
        let mut open_qualifiers = input.index.cold.open_qualifiers.clone();
        open_qualifiers.insert(qualifier.map(Box::<str>::from));
        Self::from_typed_parts_with_aliases_and_exact_precedence(
            input.columns().to_vec(),
            input.identities().to_vec(),
            input.column_types().to_vec(),
            input.index.slots.to_vec(),
            input.physical_width(),
            SchemaBuildMetadata {
                aliases: input.index.aliases.clone(),
                alias_types: input.index.cold.aliases.clone(),
                internal: input.index.executor_attributes.clone(),
                internal_types: input.index.cold.executor_attribute_types.clone(),
                score_sources: input.index.cold.score_sources.clone(),
                wildcard_hidden: input.index.cold.wildcard_hidden.clone(),
                binding_only: input.index.cold.binding_only.clone(),
                open_qualifiers,
                extra_ambiguous_unqualified: input.index.ambiguous_unqualified.clone(),
                extra_ambiguous_qualified: input.index.ambiguous_qualified.clone(),
                ..SchemaBuildMetadata::default()
            },
        )
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
