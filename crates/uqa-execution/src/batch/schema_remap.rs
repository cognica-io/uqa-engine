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
        Self::from_typed_parts_with_aliases_and_exact_precedence(
            output_names,
            identities,
            types,
            slots,
            input.physical_width(),
            SchemaBuildMetadata {
                aliases: HashMap::new(),
                alias_types: HashMap::new(),
                binding_only: HashMap::new(),
                ..SchemaBuildMetadata::default()
            },
        )
    }
}
