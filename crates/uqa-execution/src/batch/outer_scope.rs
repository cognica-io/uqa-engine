//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Correlated outer-scope schema composition.

use std::collections::{HashMap, HashSet};

use uqa_sql::ast::ColumnType;

use super::{ColumnIdentity, RowSchema, SchemaBuildMetadata, NULL_SLOT};

impl RowSchema {
    /// Overlay a statically typed correlated outer scope. The outer columns remain hidden from schema iteration and star expansion, but both qualified and unqualified lookup aliases retain their declared SQL types for expression binding.
    pub fn with_typed_outer_scope(
        input: &Self,
        outer_columns: &[(String, Option<ColumnType>)],
    ) -> Self {
        let identities = outer_columns
            .iter()
            .map(|(column, ty)| (ColumnIdentity::unqualified(column), ty.clone()))
            .collect::<Vec<_>>();
        Self::with_typed_outer_identities(input, &identities)
    }

    /// Overlay a correlated outer scope carried as structured identities.
    pub fn with_typed_outer_identities(
        input: &Self,
        outer_columns: &[(ColumnIdentity, Option<ColumnType>)],
    ) -> Self {
        let outer_base = input.physical_width();
        let current_qualifiers = input
            .index
            .qualified
            .keys()
            .chain(input.index.aliases.keys())
            .filter_map(ColumnIdentity::qualifier)
            .collect::<HashSet<_>>();
        let mut current_unqualified = input
            .identities()
            .iter()
            .map(ColumnIdentity::column)
            .collect::<HashSet<_>>();
        current_unqualified.extend(
            input
                .index
                .aliases
                .keys()
                .filter(|identity| identity.qualifier().is_none())
                .map(ColumnIdentity::column),
        );

        let mut aliases = input.index.aliases.clone();
        let mut alias_types = input.index.cold.aliases.clone();
        let mut ambiguous_qualified = input.index.ambiguous_qualified.clone();
        let mut outer_exact = HashSet::<&str>::new();
        let mut outer_qualified = HashMap::<&str, Vec<usize>>::new();
        let mut outer_qualified_counts = HashMap::<ColumnIdentity, usize>::new();
        for (identity, _) in outer_columns {
            if identity
                .qualifier()
                .is_some_and(|qualifier| !current_qualifiers.contains(qualifier))
            {
                *outer_qualified_counts.entry(identity.clone()).or_default() += 1;
            }
        }
        for (position, (identity, ty)) in outer_columns.iter().enumerate() {
            let slot = outer_base + position;
            if let Some(qualifier) = identity.qualifier() {
                if !current_qualifiers.contains(qualifier) {
                    if outer_qualified_counts.get(identity) == Some(&1) {
                        aliases.insert(identity.clone(), slot);
                        alias_types.insert(identity.clone(), ty.clone());
                    } else {
                        aliases.remove(identity);
                        alias_types.remove(identity);
                        ambiguous_qualified.insert(identity.clone());
                    }
                }
                outer_qualified
                    .entry(identity.column())
                    .or_default()
                    .push(position);
            } else {
                outer_exact.insert(identity.column());
                if !current_unqualified.contains(identity.column()) {
                    aliases.insert(identity.clone(), slot);
                    alias_types.insert(identity.clone(), ty.clone());
                }
            }
        }

        let mut outer_ambiguous = HashSet::new();
        for (column, positions) in outer_qualified {
            if current_unqualified.contains(column) || outer_exact.contains(column) {
                continue;
            }
            match positions.as_slice() {
                [position] => {
                    let identity = ColumnIdentity::unqualified(column);
                    aliases.insert(identity.clone(), outer_base + position);
                    alias_types.insert(identity, outer_columns[*position].1.clone());
                }
                [_, _, ..] => {
                    outer_ambiguous.insert(Box::<str>::from(column));
                }
                [] => {}
            }
        }

        Self::from_typed_parts_with_aliases_and_exact_precedence(
            input.columns().to_vec(),
            input.identities().to_vec(),
            input.column_types().to_vec(),
            input.index.slots.to_vec(),
            input.physical_width() + outer_columns.len(),
            SchemaBuildMetadata {
                aliases,
                alias_types,
                extra_ambiguous_unqualified: outer_ambiguous,
                extra_ambiguous_qualified: ambiguous_qualified,
                ..SchemaBuildMetadata::default()
            },
        )
    }

    /// Overlay an existing positional outer scope while preserving its complete structured lookup layout and sharing its physical value fragments.
    pub(crate) fn with_outer_schema(input: &Self, outer: &Self) -> Self {
        let outer_base = input.physical_width();
        let current_qualifiers = input
            .index
            .qualified
            .keys()
            .chain(input.index.aliases.keys())
            .filter_map(ColumnIdentity::qualifier)
            .collect::<HashSet<_>>();
        let mut current_unqualified = input
            .index
            .unqualified
            .keys()
            .map(std::convert::AsRef::as_ref)
            .collect::<HashSet<_>>();
        current_unqualified.extend(
            input
                .index
                .aliases
                .keys()
                .filter(|identity| identity.qualifier().is_none())
                .map(ColumnIdentity::column),
        );

        let shifted_slot = |slot: usize| {
            if slot == NULL_SLOT {
                NULL_SLOT
            } else {
                outer_base + slot
            }
        };
        let mut aliases = input.index.aliases.clone();
        let mut alias_types = input.index.cold.aliases.clone();
        let mut ambiguous_unqualified = input.index.ambiguous_unqualified.clone();
        let mut ambiguous_qualified = input.index.ambiguous_qualified.clone();

        let qualified_identities = outer
            .index
            .qualified
            .keys()
            .chain(
                outer
                    .index
                    .aliases
                    .keys()
                    .filter(|identity| identity.qualifier().is_some()),
            )
            .chain(outer.index.ambiguous_qualified.iter())
            .cloned()
            .collect::<HashSet<_>>();
        for identity in qualified_identities {
            let qualifier = identity
                .qualifier()
                .expect("qualified outer identity has a qualifier");
            if current_qualifiers.contains(qualifier) {
                continue;
            }
            if outer.qualified_column_is_ambiguous(qualifier, identity.column()) {
                aliases.remove(&identity);
                alias_types.remove(&identity);
                ambiguous_qualified.insert(identity);
                continue;
            }
            let slot = outer
                .qualified_slot(qualifier, identity.column())
                .map_or(NULL_SLOT, shifted_slot);
            let ty = outer.qualified_type(qualifier, identity.column()).cloned();
            aliases.insert(identity.clone(), slot);
            alias_types.insert(identity, ty);
        }

        let outer_unqualified = outer
            .index
            .unqualified
            .keys()
            .map(std::convert::AsRef::as_ref)
            .chain(
                outer
                    .index
                    .aliases
                    .keys()
                    .filter(|identity| identity.qualifier().is_none())
                    .map(ColumnIdentity::column),
            )
            .chain(
                outer
                    .index
                    .ambiguous_unqualified
                    .iter()
                    .map(std::convert::AsRef::as_ref),
            )
            .collect::<HashSet<_>>();
        for column in outer_unqualified {
            if current_unqualified.contains(column) {
                continue;
            }
            let identity = ColumnIdentity::unqualified(column);
            if outer.column_is_ambiguous(column) {
                aliases.remove(&identity);
                alias_types.remove(&identity);
                ambiguous_unqualified.insert(Box::<str>::from(column));
                continue;
            }
            let slot = outer.column_slot(column).map_or(NULL_SLOT, shifted_slot);
            aliases.insert(identity.clone(), slot);
            alias_types.insert(identity, outer.type_of(column).cloned());
        }

        Self::from_typed_parts_with_aliases_and_exact_precedence(
            input.columns().to_vec(),
            input.identities().to_vec(),
            input.column_types().to_vec(),
            input.index.slots.to_vec(),
            input.physical_width() + outer.physical_width(),
            SchemaBuildMetadata {
                aliases,
                alias_types,
                extra_ambiguous_unqualified: ambiguous_unqualified,
                extra_ambiguous_qualified: ambiguous_qualified,
                ..SchemaBuildMetadata::default()
            },
        )
    }
}
