//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Logical identity remapping over shared physical row slots.

use std::collections::HashMap;

use uqa_sql::ast::ColumnType;

use super::{ColumnIdentity, RowSchema, SchemaBuildMetadata, NULL_SLOT};

impl RowSchema {
    /// Select explicit physical slots with fresh public identities. This is
    /// the structural equivalent of executor target entries whose values may
    /// come from anonymous computed slots rather than SQL-named columns.
    pub(crate) fn remap_typed_physical_identities(
        input: &Self,
        columns: &[(String, ColumnIdentity, usize, Option<ColumnType>)],
        aliases: &[(ColumnIdentity, usize, Option<ColumnType>)],
    ) -> Self {
        let output_names = columns
            .iter()
            .map(|(output, _, _, _)| output.clone())
            .collect();
        let identities = columns
            .iter()
            .map(|(_, identity, _, _)| identity.clone())
            .collect();
        let slots = columns
            .iter()
            .map(|(_, _, physical, _)| {
                if *physical < input.physical_width() {
                    *physical
                } else {
                    NULL_SLOT
                }
            })
            .collect();
        let types = columns.iter().map(|(_, _, _, ty)| ty.clone()).collect();
        let wildcard_hidden = columns
            .iter()
            .enumerate()
            .filter_map(|(output, (_, _, physical, _))| {
                input
                    .index
                    .cold
                    .wildcard_hidden
                    .iter()
                    .any(|logical| input.slot(*logical) == Some(*physical))
                    .then_some(output)
            })
            .collect();
        let mut lookup_aliases = input.index.aliases.clone();
        let mut alias_types = input.index.cold.aliases.clone();
        for (identity, physical, ty) in aliases {
            lookup_aliases.insert(
                identity.clone(),
                if *physical < input.physical_width() {
                    *physical
                } else {
                    NULL_SLOT
                },
            );
            alias_types.insert(identity.clone(), ty.clone());
        }
        Self::from_typed_parts_with_aliases_and_exact_precedence(
            output_names,
            identities,
            types,
            slots,
            input.physical_width(),
            SchemaBuildMetadata {
                aliases: lookup_aliases,
                alias_types,
                internal: input.index.internal.clone(),
                internal_types: input.index.cold.internal.clone(),
                score_sources: input.index.cold.score_sources.clone(),
                wildcard_hidden,
                binding_only: input.index.cold.binding_only.clone(),
                ..SchemaBuildMetadata::default()
            },
        )
    }

    /// Select logical positions with explicit public labels, SQL identities, and types while preserving hidden aliases and physical fragments.
    pub(crate) fn remap_typed_identities(
        input: &Self,
        columns: &[(String, ColumnIdentity, usize, Option<ColumnType>)],
        aliases: &[(ColumnIdentity, usize)],
    ) -> Self {
        let output_names = columns
            .iter()
            .map(|(output, _, _, _)| output.clone())
            .collect();
        let identities = columns
            .iter()
            .map(|(_, identity, _, _)| identity.clone())
            .collect();
        let slots = columns
            .iter()
            .map(|(_, _, logical, _)| input.slot(*logical).unwrap_or(NULL_SLOT))
            .collect();
        let types = columns.iter().map(|(_, _, _, ty)| ty.clone()).collect();
        let wildcard_hidden = columns
            .iter()
            .enumerate()
            .filter_map(|(output, (_, _, logical, _))| {
                input
                    .index
                    .cold
                    .wildcard_hidden
                    .contains(logical)
                    .then_some(output)
            })
            .collect();
        let mut lookup_aliases = input.index.aliases.clone();
        let mut alias_types = input.index.cold.aliases.clone();
        for (identity, logical) in aliases {
            lookup_aliases.insert(identity.clone(), input.slot(*logical).unwrap_or(NULL_SLOT));
            alias_types.insert(identity.clone(), input.column_type(*logical).cloned());
        }
        Self::from_typed_parts_with_aliases_and_exact_precedence(
            output_names,
            identities,
            types,
            slots,
            input.physical_width(),
            SchemaBuildMetadata {
                aliases: lookup_aliases,
                alias_types,
                internal: input.index.internal.clone(),
                internal_types: input.index.cold.internal.clone(),
                score_sources: input.index.cold.score_sources.clone(),
                wildcard_hidden,
                binding_only: input.index.cold.binding_only.clone(),
                ..SchemaBuildMetadata::default()
            },
        )
    }

    /// Select logical positions with fresh SQL identities while deliberately dropping every input lookup alias. A relation alias on a complete parenthesized JOIN uses this boundary because `PostgreSQL` hides all names exposed by the joined inputs.
    pub(crate) fn remap_typed_identities_without_input_aliases(
        input: &Self,
        columns: &[(String, ColumnIdentity, usize, Option<ColumnType>)],
    ) -> Self {
        let output_names = columns
            .iter()
            .map(|(output, _, _, _)| output.clone())
            .collect();
        let identities = columns
            .iter()
            .map(|(_, identity, _, _)| identity.clone())
            .collect();
        let slots = columns
            .iter()
            .map(|(_, _, logical, _)| input.slot(*logical).unwrap_or(NULL_SLOT))
            .collect();
        let types = columns.iter().map(|(_, _, _, ty)| ty.clone()).collect();
        let wildcard_hidden = columns
            .iter()
            .enumerate()
            .filter_map(|(output, (_, _, logical, _))| {
                input
                    .index
                    .cold
                    .wildcard_hidden
                    .contains(logical)
                    .then_some(output)
            })
            .collect();
        Self::from_typed_parts_with_aliases_and_exact_precedence(
            output_names,
            identities,
            types,
            slots,
            input.physical_width(),
            SchemaBuildMetadata {
                aliases: HashMap::new(),
                alias_types: HashMap::new(),
                internal: input.index.internal.clone(),
                internal_types: input.index.cold.internal.clone(),
                score_sources: input.index.cold.score_sources.clone(),
                wildcard_hidden,
                binding_only: HashMap::new(),
                ..SchemaBuildMetadata::default()
            },
        )
    }
}
