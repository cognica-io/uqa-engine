//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Static SQL name and type binding over a physical row schema.

use uqa_sql::ast::{ColumnType, InternalColumnRef};

use super::{ColumnIdentity, RowSchema, SchemaBuildMetadata, ScoreSource, NULL_SLOT};

impl RowSchema {
    /// Bind every SQL-visible column to one relation qualifier while preserving executor-only internal attributes and rebinding carried retrieval scores to the same relation boundary.
    pub fn with_relation_qualifier(input: &Self, qualifier: &str) -> Self {
        let identities = input
            .columns()
            .iter()
            .cloned()
            .map(|column| ColumnIdentity::qualified(qualifier, column))
            .collect();
        let score_sources = input
            .index
            .cold
            .score_sources
            .iter()
            .map(|source| ScoreSource {
                qualifier: Some(Box::<str>::from(qualifier)),
                column: source.column,
            })
            .collect();
        Self::from_typed_parts_with_aliases_and_exact_precedence(
            input.columns().to_vec(),
            identities,
            input.column_types().to_vec(),
            input.index.slots.to_vec(),
            input.physical_width(),
            SchemaBuildMetadata {
                internal: input.index.executor_attributes.clone(),
                internal_types: input.index.cold.executor_attribute_types.clone(),
                score_sources,
                wildcard_hidden: input.index.cold.wildcard_hidden.clone(),
                ..SchemaBuildMetadata::default()
            },
        )
    }

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

    /// Iterate over hidden lookup aliases that resolve to physical value slots while retaining their declared SQL types.
    pub fn typed_physical_alias_identities(
        &self,
    ) -> impl Iterator<Item = (&ColumnIdentity, Option<&ColumnType>)> {
        self.index.aliases.keys().map(|identity| {
            (
                identity,
                self.index
                    .cold
                    .aliases
                    .get(identity)
                    .and_then(Option::as_ref),
            )
        })
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
                internal: input.index.executor_attributes.clone(),
                internal_types: input.index.cold.executor_attribute_types.clone(),
                score_sources: input.index.cold.score_sources.clone(),
                wildcard_hidden: input.index.cold.wildcard_hidden.clone(),
                binding_only: input.index.cold.binding_only.clone(),
                ..SchemaBuildMetadata::default()
            },
        )
    }

    /// Add hidden SQL lookup identities that point at explicit physical slots.
    /// Unlike [`Self::with_identity_aliases`], these slots do not need a
    /// SQL-visible logical column owner.
    pub fn with_physical_identity_aliases(
        input: &Self,
        aliases: &[(ColumnIdentity, usize, Option<ColumnType>)],
    ) -> Self {
        let mut lookup_aliases = input.index.aliases.clone();
        let mut alias_types = input.index.cold.aliases.clone();
        for (identity, slot, ty) in aliases {
            assert!(
                *slot < input.physical_width(),
                "physical identity alias is outside row width"
            );
            lookup_aliases.insert(identity.clone(), *slot);
            alias_types.insert(identity.clone(), ty.clone());
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
                internal: input.index.executor_attributes.clone(),
                internal_types: input.index.cold.executor_attribute_types.clone(),
                score_sources: input.index.cold.score_sources.clone(),
                wildcard_hidden: input.index.cold.wildcard_hidden.clone(),
                binding_only: input.index.cold.binding_only.clone(),
                ..SchemaBuildMetadata::default()
            },
        )
    }

    /// Add executor-only relation attributes at explicit physical slots.
    pub fn with_physical_internal_aliases(
        input: &Self,
        aliases: &[(InternalColumnRef, usize, Option<ColumnType>)],
    ) -> Self {
        let mut internal = input.index.executor_attributes.clone();
        let mut internal_types = input.index.cold.executor_attribute_types.clone();
        for (column, slot, ty) in aliases {
            assert!(
                *slot < input.physical_width(),
                "physical internal alias is outside row width"
            );
            internal.insert(*column, *slot);
            internal_types.insert(*column, ty.clone());
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
                internal,
                internal_types,
                score_sources: input.index.cold.score_sources.clone(),
                wildcard_hidden: input.index.cold.wildcard_hidden.clone(),
                binding_only: input.index.cold.binding_only.clone(),
                ..SchemaBuildMetadata::default()
            },
        )
    }

    /// Mark source-owned logical metadata attributes as explicitly
    /// addressable but absent from `*` expansion. The identity is positional,
    /// so an ordinary user column with the same text remains visible.
    pub fn with_wildcard_hidden_positions(
        input: &Self,
        positions: impl IntoIterator<Item = usize>,
    ) -> Self {
        let wildcard_hidden = positions.into_iter().collect();
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
                wildcard_hidden,
                binding_only: input.index.cold.binding_only.clone(),
                ..SchemaBuildMetadata::default()
            },
        )
    }

    #[must_use]
    pub fn internal_slot(&self, column: InternalColumnRef) -> Option<usize> {
        self.index
            .executor_attributes
            .get(&column)
            .copied()
            .filter(|slot| *slot != NULL_SLOT)
    }

    #[must_use]
    pub fn internal_type(&self, column: InternalColumnRef) -> Option<&ColumnType> {
        self.index
            .cold
            .executor_attribute_types
            .get(&column)
            .and_then(Option::as_ref)
    }

    /// Mark an existing internal attribute as the score carried by one retrieval relation. This semantic tag is schema metadata and never enters SQL name lookup or wildcard expansion.
    pub fn with_score_source(
        input: &Self,
        qualifier: Option<&str>,
        column: InternalColumnRef,
    ) -> Self {
        assert!(
            input.index.executor_attributes.contains_key(&column),
            "score source must reference an existing internal attribute"
        );
        let mut score_sources = input.index.cold.score_sources.clone();
        score_sources.retain(|source| source.column != column);
        score_sources.push(ScoreSource {
            qualifier: qualifier.map(Box::<str>::from),
            column,
        });
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
                score_sources,
                wildcard_hidden: input.index.cold.wildcard_hidden.clone(),
                binding_only: input.index.cold.binding_only.clone(),
                ..SchemaBuildMetadata::default()
            },
        )
    }

    /// Rebind every carried score source at a relation-alias boundary while retaining its opaque internal attribute identity.
    pub fn with_rebound_score_sources(input: &Self, qualifier: Option<&str>) -> Self {
        let score_sources = input
            .index
            .cold
            .score_sources
            .iter()
            .map(|source| ScoreSource {
                qualifier: qualifier.map(Box::<str>::from),
                column: source.column,
            })
            .collect();
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
                score_sources,
                wildcard_hidden: input.index.cold.wildcard_hidden.clone(),
                binding_only: input.index.cold.binding_only.clone(),
                ..SchemaBuildMetadata::default()
            },
        )
    }

    fn matching_score_sources<'a>(
        &'a self,
        qualifier: Option<&'a str>,
    ) -> impl Iterator<Item = InternalColumnRef> + 'a {
        self.index
            .cold
            .score_sources
            .iter()
            .filter(move |source| {
                qualifier.is_none_or(|qualifier| source.qualifier.as_deref() == Some(qualifier))
            })
            .map(move |source| source.column)
    }

    #[must_use]
    pub fn score_source_column(&self, qualifier: Option<&str>) -> Option<InternalColumnRef> {
        let mut columns = self.matching_score_sources(qualifier);
        let column = columns.next()?;
        columns.next().is_none().then_some(column)
    }

    #[must_use]
    pub fn score_source_is_ambiguous(&self, qualifier: Option<&str>) -> bool {
        let mut columns = self.matching_score_sources(qualifier);
        columns.next().is_some() && columns.next().is_some()
    }

    #[must_use]
    pub fn score_source_slot(&self, qualifier: Option<&str>) -> Option<usize> {
        self.score_source_column(qualifier)
            .and_then(|column| self.internal_slot(column))
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
                internal: input.index.executor_attributes.clone(),
                internal_types: input.index.cold.executor_attribute_types.clone(),
                score_sources: input.index.cold.score_sources.clone(),
                wildcard_hidden: input.index.cold.wildcard_hidden.clone(),
                binding_only,
                extra_ambiguous_unqualified: input.index.ambiguous_unqualified.clone(),
                extra_ambiguous_qualified: input.index.ambiguous_qualified.clone(),
                ..SchemaBuildMetadata::default()
            },
        )
    }

    /// Add binding-only identities and preserve collisions as ambiguous names. This models SQL scopes that expose a hidden generated column for explicit lookup while deliberately excluding it from wildcard expansion.
    pub fn with_typed_conflicting_virtual_identities(
        input: &Self,
        identities: &[(ColumnIdentity, Option<ColumnType>)],
    ) -> Self {
        let mut binding_only = input.index.cold.binding_only.clone();
        let mut ambiguous_unqualified = input.index.ambiguous_unqualified.clone();
        let mut ambiguous_qualified = input.index.ambiguous_qualified.clone();
        for (identity, ty) in identities {
            if input.column_is_ambiguous(identity.column())
                || input.has_unqualified_column(identity.column())
            {
                ambiguous_unqualified.insert(Box::<str>::from(identity.column()));
            }
            if let Some(qualifier) = identity.qualifier() {
                if input.has_qualified_column(qualifier, identity.column()) {
                    ambiguous_qualified.insert(identity.clone());
                }
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
                internal: input.index.executor_attributes.clone(),
                internal_types: input.index.cold.executor_attribute_types.clone(),
                score_sources: input.index.cold.score_sources.clone(),
                wildcard_hidden: input.index.cold.wildcard_hidden.clone(),
                binding_only,
                extra_ambiguous_unqualified: ambiguous_unqualified,
                extra_ambiguous_qualified: ambiguous_qualified,
                ..SchemaBuildMetadata::default()
            },
        )
    }
}
