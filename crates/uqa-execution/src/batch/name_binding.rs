//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Static SQL name and type binding over a physical row schema.

use uqa_sql::ast::ColumnType;

use super::{ColumnIdentity, RowSchema, SchemaBuildMetadata, NULL_SLOT};

impl RowSchema {
    /// Whether a visible or hidden lookup identity belongs to `qualifier`.
    #[must_use]
    pub fn has_qualifier(&self, qualifier: &str) -> bool {
        self.index
            .identities
            .iter()
            .chain(self.index.aliases.keys())
            .chain(self.index.cold.binding_only.keys())
            .any(|identity| identity.qualifier() == Some(qualifier))
    }

    /// Whether a visible, aliased, or static binding-only identity contains this exact unqualified column, independently of its declared type.
    #[must_use]
    pub fn has_unqualified_column(&self, column: &str) -> bool {
        let identity = ColumnIdentity::unqualified(column);
        self.index.unqualified.contains_key(column)
            || self.index.aliases.contains_key(&identity)
            || self.index.cold.binding_only.contains_key(&identity)
    }

    /// Whether a visible or hidden lookup identity contains this exact qualified column, independently of its static type or ambiguity.
    #[must_use]
    pub fn has_qualified_column(&self, qualifier: &str, column: &str) -> bool {
        let identity = ColumnIdentity::qualified(qualifier, column);
        self.index.qualified.contains_key(&identity)
            || self.index.aliases.contains_key(&identity)
            || self.index.cold.binding_only.contains_key(&identity)
    }

    /// Iterate over static name-binding identities that deliberately have no physical value slot or wildcard visibility.
    pub fn typed_virtual_identities(
        &self,
    ) -> impl Iterator<Item = (&ColumnIdentity, Option<&ColumnType>)> {
        self.index
            .cold
            .binding_only
            .iter()
            .map(|(identity, ty)| (identity, ty.as_ref()))
    }

    /// Resolve an unqualified logical identity to its static type.
    pub fn type_of(&self, name: &str) -> Option<&ColumnType> {
        if self.index.ambiguous_unqualified.contains(name) {
            return None;
        }
        self.index
            .unqualified
            .get(name)
            .and_then(|logical| self.column_type(*logical))
            .or_else(|| {
                self.index
                    .cold
                    .aliases
                    .get(&ColumnIdentity::unqualified(name))
                    .and_then(Option::as_ref)
            })
            .or_else(|| {
                self.index
                    .cold
                    .binding_only
                    .get(&ColumnIdentity::unqualified(name))
                    .and_then(Option::as_ref)
            })
    }

    /// Resolve a qualified logical identity to its static type.
    pub fn qualified_type(&self, qualifier: &str, column: &str) -> Option<&ColumnType> {
        let identity = ColumnIdentity::qualified(qualifier, column);
        if self.index.ambiguous_qualified.contains(&identity) {
            return None;
        }
        self.index
            .qualified
            .get(&identity)
            .and_then(|logical| self.column_type(*logical))
            .or_else(|| {
                self.index
                    .cold
                    .aliases
                    .get(&identity)
                    .and_then(Option::as_ref)
            })
            .or_else(|| {
                self.index
                    .cold
                    .binding_only
                    .get(&identity)
                    .and_then(Option::as_ref)
            })
    }

    pub fn column_is_ambiguous(&self, name: &str) -> bool {
        self.index.ambiguous_unqualified.contains(name)
    }

    pub fn qualified_column_is_ambiguous(&self, qualifier: &str, column: &str) -> bool {
        self.index
            .ambiguous_qualified
            .contains(&ColumnIdentity::qualified(qualifier, column))
    }

    /// Add hidden structured lookup identities for existing logical positions.
    pub fn with_identity_aliases(input: &Self, aliases: &[(ColumnIdentity, usize)]) -> Self {
        let mut lookup_aliases = input.index.aliases.clone();
        let mut alias_types = input.index.cold.aliases.clone();
        for (identity, logical) in aliases {
            lookup_aliases.insert(identity.clone(), input.slot(*logical).unwrap_or(NULL_SLOT));
            alias_types.insert(identity.clone(), input.column_type(*logical).cloned());
        }
        Self::from_typed_parts_with_aliases_and_exact_precedence(
            input.columns().to_vec(),
            input.identities().to_vec(),
            input.column_types().to_vec(),
            input.index.slots.to_vec(),
            input.physical_width(),
            SchemaBuildMetadata {
                aliases: lookup_aliases,
                alias_types,
                binding_only: input.index.cold.binding_only.clone(),
                ..SchemaBuildMetadata::default()
            },
        )
    }

    /// Add statically typed hidden lookup identities that have no physical value slot. Visible columns and star expansion remain unchanged, while binders can resolve the identities and their declared SQL types.
    pub fn with_typed_virtual_identities(
        input: &Self,
        identities: &[(ColumnIdentity, Option<ColumnType>)],
    ) -> Self {
        let mut binding_only = input.index.cold.binding_only.clone();
        for (identity, ty) in identities {
            let already_visible = match identity.qualifier() {
                Some(qualifier) => input.has_qualified_column(qualifier, identity.column()),
                None => {
                    input.column_is_ambiguous(identity.column())
                        || input.has_unqualified_column(identity.column())
                }
            };
            if already_visible {
                continue;
            }
            binding_only
                .entry(identity.clone())
                .or_insert_with(|| ty.clone());
        }
        Self::from_typed_parts_with_aliases_and_exact_precedence(
            input.columns().to_vec(),
            input.identities().to_vec(),
            input.column_types().to_vec(),
            input.index.slots.to_vec(),
            input.physical_width(),
            SchemaBuildMetadata {
                aliases: input.index.aliases.clone(),
                alias_types: input.index.cold.aliases.clone(),
                binding_only,
                ..SchemaBuildMetadata::default()
            },
        )
    }
}
