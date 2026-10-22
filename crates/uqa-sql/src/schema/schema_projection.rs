//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Projection, canonicalization, and position remapping for row schemas.

use super::{
    ColumnIdentity, ColumnType, HashMap, PhysicalLayout, ProjectedSlot, RowSchema,
    SchemaBuildMetadata, SchemaLayoutError, SchemaLayoutResult, SchemaMap, ScoreSource, NULL_SLOT,
};

impl RowSchema {
    /// Capture positional outputs through opaque identities without retaining their SQL names or copying values. Existing internal score and record metadata remain attached to their physical slots. An implicit NULL output owns one appended NULL slot, which the physical selector must materialize.
    pub fn capture_output(input: &Self, relation: crate::ast::InternalRelationId) -> Self {
        let mut has_null = false;
        let attributes = (0..input.len())
            .map(|position| {
                (
                    relation.column(position),
                    input.column_type(position).cloned(),
                    input.physical_slot(position).map_or_else(
                        || {
                            has_null = true;
                            ProjectedSlot::Computed {
                                position: 0,
                                collation: input.column_collation(position),
                            }
                        },
                        |physical| ProjectedSlot::BoundInput {
                            physical: Some(physical),
                            collation: input.column_collation(position),
                        },
                    ),
                )
            })
            .collect();
        Self::project_with_sources(input, Vec::new(), attributes, usize::from(has_null), false)
    }

    /// Remove selected executor-only identities after their consumer has run.
    /// The physical fragments remain shareable until the next canonical
    /// boundary, where now-unreferenced slots are naturally discarded.
    pub fn without_internal_attributes(
        input: &Self,
        columns: &[crate::ast::InternalColumnRef],
    ) -> Self {
        let mut internal = input.index.executor_attributes.clone();
        let mut internal_types = input.index.cold.executor_attribute_types.clone();
        for column in columns {
            internal.remove(column);
            internal_types.remove(column);
        }
        let score_sources = input
            .index
            .cold
            .score_sources
            .iter()
            .filter(|source| internal.contains_key(&source.column))
            .cloned()
            .collect();
        Self::from_typed_parts_with_aliases_and_exact_precedence(
            input.columns().to_vec(),
            input.identities().to_vec(),
            input.column_types().to_vec(),
            input.index.slots.to_vec(),
            input.physical_width(),
            SchemaBuildMetadata {
                collations: input.index.cold.collations.clone(),
                record_fields: input.index.cold.record_fields.clone(),
                aliases: input.index.aliases.clone(),
                alias_types: input.index.cold.aliases.clone(),
                internal,
                internal_types,
                score_sources,
                wildcard_hidden: input.index.cold.wildcard_hidden.clone(),
                binding_only: input.index.cold.binding_only.clone(),
                row_types: input.index.cold.row_types.clone(),
                open_qualifiers: input.index.cold.open_qualifiers.clone(),
                ..SchemaBuildMetadata::default()
            },
        )
    }

    /// Select and optionally rename logical columns while retaining the
    /// child's physical fragments.
    pub fn select(input: &Self, columns: &[(String, String)]) -> Self {
        let mut collations = input.index.cold.collations.clone();
        collations.clear_columns();
        for (position, (_, source)) in columns.iter().enumerate() {
            let oid = input
                .position(source)
                .and_then(|position| input.inherited_column_collation(position));
            collations.column(position, oid, input.exact_type(source));
        }
        let output_names = columns
            .iter()
            .map(|(output, _)| output.clone())
            .collect::<Vec<_>>();
        let slots = columns
            .iter()
            .map(|(_, source)| input.exact_slot(source).unwrap_or(NULL_SLOT))
            .collect();
        let types = columns
            .iter()
            .map(|(_, source)| input.exact_type(source).cloned())
            .collect();
        let identities = output_names
            .iter()
            .cloned()
            .map(ColumnIdentity::unqualified)
            .collect();
        Self::from_typed_parts_with_aliases_and_exact_precedence(
            output_names,
            identities,
            types,
            slots,
            input.physical_width(),
            SchemaBuildMetadata {
                collations,
                record_fields: input.index.cold.record_fields.clone(),
                aliases: SchemaMap::default(),
                alias_types: SchemaMap::default(),
                internal: input.index.executor_attributes.clone(),
                internal_types: input.index.cold.executor_attribute_types.clone(),
                score_sources: input.index.cold.score_sources.clone(),
                binding_only: SchemaMap::default(),
                ..SchemaBuildMetadata::default()
            },
        )
    }

    /// Build a scalar-projection schema without rebuilding direct input values. A non-pass-through projection hides child identities logically while retaining their physical fragments; an appending projection preserves the child schema and replaces duplicate labels with `PostgreSQL` map-insertion semantics.
    pub fn project_with_sources(
        input: &Self,
        projected: Vec<(String, Option<ColumnType>, ProjectedSlot)>,
        projected_internal: Vec<(
            crate::ast::InternalColumnRef,
            Option<ColumnType>,
            ProjectedSlot,
        )>,
        computed_count: usize,
        pass_through: bool,
    ) -> Self {
        let mut collations = input.index.cold.collations.clone();
        let resolve_slot = |source: ProjectedSlot| match source {
            ProjectedSlot::Input(slot) | ProjectedSlot::BoundInput { physical: slot, .. } => {
                slot.unwrap_or(NULL_SLOT)
            }
            ProjectedSlot::Computed { position, .. } => input.physical_width() + position,
        };
        let source_collation = |source: ProjectedSlot| match source {
            ProjectedSlot::Input(Some(slot)) => input.inherited_physical_collation(slot),
            ProjectedSlot::BoundInput { collation, .. }
            | ProjectedSlot::Computed { collation, .. } => collation,
            ProjectedSlot::Input(None) => None,
        };
        let physical_width = input.physical_width() + computed_count;
        let mut internal = input.index.executor_attributes.clone();
        let mut internal_types = input.index.cold.executor_attribute_types.clone();
        for (column, ty, source) in projected_internal {
            collations.internal(column, source_collation(source), ty.as_ref());
            internal.insert(column, resolve_slot(source));
            internal_types.insert(column, ty);
        }

        if pass_through {
            let mut columns = input.columns().to_vec();
            let mut identities = input.identities().to_vec();
            let mut types = input.column_types().to_vec();
            let mut slots = input.index.slots.to_vec();
            let mut wildcard_hidden = input.index.cold.wildcard_hidden.clone();
            for (name, ty, source) in projected {
                let slot = resolve_slot(source);
                if let Some(position) = columns.iter().position(|column| column == &name) {
                    slots[position] = slot;
                    identities[position] = ColumnIdentity::unqualified(name);
                    collations.column(position, source_collation(source), ty.as_ref());
                    types[position] = ty;
                    wildcard_hidden.remove(&position);
                } else {
                    identities.push(ColumnIdentity::unqualified(name.clone()));
                    columns.push(name);
                    collations.column(types.len(), source_collation(source), ty.as_ref());
                    types.push(ty);
                    slots.push(slot);
                }
            }
            return Self::from_typed_parts_with_aliases_and_exact_precedence(
                columns,
                identities,
                types,
                slots,
                physical_width,
                SchemaBuildMetadata {
                    collations,
                    record_fields: input.index.cold.record_fields.clone(),
                    aliases: input.index.aliases.clone(),
                    alias_types: input.index.cold.aliases.clone(),
                    internal,
                    internal_types,
                    score_sources: input.index.cold.score_sources.clone(),
                    wildcard_hidden: wildcard_hidden.into_iter().collect(),
                    binding_only: input.index.cold.binding_only.clone(),
                    row_types: input.index.cold.row_types.clone(),
                    open_qualifiers: input.index.cold.open_qualifiers.clone(),
                    ..SchemaBuildMetadata::default()
                },
            );
        }

        collations.clear_columns();
        let mut columns = Vec::with_capacity(projected.len());
        let mut identities = Vec::with_capacity(projected.len());
        let mut types = Vec::with_capacity(projected.len());
        let mut slots = Vec::with_capacity(projected.len());
        for (name, ty, source) in projected {
            slots.push(resolve_slot(source));
            identities.push(ColumnIdentity::unqualified(name.clone()));
            columns.push(name);
            collations.column(types.len(), source_collation(source), ty.as_ref());
            types.push(ty);
        }
        Self::from_typed_parts_with_aliases_and_exact_precedence(
            columns,
            identities,
            types,
            slots,
            physical_width,
            SchemaBuildMetadata {
                collations,
                record_fields: input.index.cold.record_fields.clone(),
                aliases: SchemaMap::default(),
                alias_types: SchemaMap::default(),
                internal,
                internal_types,
                score_sources: input.index.cold.score_sources.clone(),
                binding_only: SchemaMap::default(),
                ..SchemaBuildMetadata::default()
            },
        )
    }

    /// Build a compact positional layout for a blocking or spill boundary.
    /// Logical columns and hidden lookup aliases are remapped to a deduplicated
    /// list of referenced physical slots; projecting a row through the returned
    /// slot list shares its existing value fragments without cloning values.
    pub fn canonical_projection(&self) -> (Self, Vec<usize>) {
        fn remap_slot(
            slot: usize,
            source_slots: &mut Vec<usize>,
            positions: &mut HashMap<usize, usize>,
        ) -> usize {
            if slot == NULL_SLOT {
                return NULL_SLOT;
            }
            if let Some(position) = positions.get(&slot) {
                return *position;
            }
            let position = source_slots.len();
            source_slots.push(slot);
            positions.insert(slot, position);
            position
        }

        let mut source_slots = Vec::new();
        let mut positions = HashMap::new();
        let slots = self
            .index
            .slots
            .iter()
            .map(|slot| remap_slot(*slot, &mut source_slots, &mut positions))
            .collect();
        let mut source_aliases = self.index.aliases.iter().collect::<Vec<_>>();
        source_aliases.sort_unstable_by_key(|(left, _)| *left);
        let aliases = source_aliases
            .into_iter()
            .map(|(name, slot)| {
                (
                    name.clone(),
                    remap_slot(*slot, &mut source_slots, &mut positions),
                )
            })
            .collect();
        let mut source_internal = self.index.executor_attributes.iter().collect::<Vec<_>>();
        source_internal.sort_unstable_by_key(|(column, _)| **column);
        let internal = source_internal
            .into_iter()
            .map(|(column, slot)| {
                (
                    *column,
                    remap_slot(*slot, &mut source_slots, &mut positions),
                )
            })
            .collect();
        (
            Self::from_typed_parts_with_aliases_and_exact_precedence(
                self.columns().to_vec(),
                self.identities().to_vec(),
                self.column_types().to_vec(),
                slots,
                source_slots.len(),
                SchemaBuildMetadata {
                    collations: self.index.cold.collations.clone(),
                    record_fields: self
                        .index
                        .cold
                        .record_fields
                        .iter()
                        .filter_map(|(slot, fields)| {
                            positions.get(slot).map(|mapped| (*mapped, fields.clone()))
                        })
                        .collect(),
                    aliases,
                    alias_types: self.index.cold.aliases.clone(),
                    internal,
                    internal_types: self.index.cold.executor_attribute_types.clone(),
                    score_sources: self.index.cold.score_sources.clone(),
                    wildcard_hidden: self.index.cold.wildcard_hidden.clone(),
                    binding_only: self.index.cold.binding_only.clone(),
                    row_types: self.index.cold.row_types.clone(),
                    open_qualifiers: self.index.cold.open_qualifiers.clone(),
                    ..SchemaBuildMetadata::default()
                },
            ),
            source_slots,
        )
    }

    /// Rebuild a schema decoded from a positional spill record.
    ///
    /// `None` represents a logical or alias identity whose source was absent
    /// and therefore resolves to SQL NULL. Every physical slot is validated
    /// before the derived lookup indexes are constructed.
    #[expect(
        clippy::too_many_lines,
        reason = "projection keeps schema and physical column positions aligned"
    )]
    pub fn from_physical_layout(layout: PhysicalLayout) -> SchemaLayoutResult<Self> {
        let PhysicalLayout {
            columns,
            identities,
            types,
            slots,
            physical_width,
            aliases,
            internal,
            score_sources,
            wildcard_hidden,
        } = layout;
        if columns.len() != slots.len() {
            return Err(SchemaLayoutError(format!(
                "physical schema has {} columns but {} logical slots",
                columns.len(),
                slots.len()
            )));
        }
        if columns.len() != types.len() {
            return Err(SchemaLayoutError(format!(
                "physical schema has {} columns but {} logical types",
                columns.len(),
                types.len()
            )));
        }
        if columns.len() != identities.len() {
            return Err(SchemaLayoutError(format!(
                "physical schema has {} columns but {} logical identities",
                columns.len(),
                identities.len()
            )));
        }
        if wildcard_hidden
            .iter()
            .any(|position| *position >= columns.len())
        {
            return Err(SchemaLayoutError(
                "physical schema wildcard-hidden position is outside logical width".into(),
            ));
        }
        let slots = slots
            .into_iter()
            .map(|slot| match slot {
                Some(slot) if slot < physical_width => Ok(slot),
                Some(slot) => Err(SchemaLayoutError(format!(
                    "physical schema logical slot {slot} is outside width {physical_width}"
                ))),
                None => Ok(NULL_SLOT),
            })
            .collect::<SchemaLayoutResult<Vec<_>>>()?;
        let mut lookup_aliases =
            SchemaMap::with_capacity_and_hasher(aliases.len(), ahash::RandomState::default());
        let mut alias_types =
            SchemaMap::with_capacity_and_hasher(aliases.len(), ahash::RandomState::default());
        for (identity, slot, ty) in aliases {
            let slot = match slot {
                Some(slot) if slot < physical_width => slot,
                Some(slot) => {
                    return Err(SchemaLayoutError(format!(
                        "physical schema alias `{identity:?}` slot {slot} is outside width {physical_width}"
                    )))
                }
                None => NULL_SLOT,
            };
            if lookup_aliases.insert(identity.clone(), slot).is_some() {
                return Err(SchemaLayoutError(format!(
                    "physical schema contains duplicate alias `{identity:?}`"
                )));
            }
            alias_types.insert(identity, ty);
        }
        let mut internal_slots =
            SchemaMap::with_capacity_and_hasher(internal.len(), ahash::RandomState::default());
        let mut internal_types =
            SchemaMap::with_capacity_and_hasher(internal.len(), ahash::RandomState::default());
        for (column, slot, ty) in internal {
            let slot = match slot {
                Some(slot) if slot < physical_width => slot,
                Some(slot) => {
                    return Err(SchemaLayoutError(format!(
                        "physical schema internal attribute `{column:?}` slot {slot} is outside width {physical_width}"
                    )))
                }
                None => NULL_SLOT,
            };
            if internal_slots.insert(column, slot).is_some() {
                return Err(SchemaLayoutError(format!(
                    "physical schema contains duplicate internal attribute `{column:?}`"
                )));
            }
            internal_types.insert(column, ty);
        }
        let score_sources = score_sources
            .into_iter()
            .map(|(qualifier, column)| {
                if !internal_slots.contains_key(&column) {
                    return Err(SchemaLayoutError(format!(
                        "physical schema score source references missing internal attribute `{column:?}`"
                    )));
                }
                Ok(ScoreSource {
                    qualifier: qualifier.map(Box::<str>::from),
                    column,
                })
            })
            .collect::<SchemaLayoutResult<Vec<_>>>()?;
        Ok(Self::from_typed_parts_with_aliases_and_exact_precedence(
            columns,
            identities,
            types,
            slots,
            physical_width,
            SchemaBuildMetadata {
                aliases: lookup_aliases,
                alias_types,
                internal: internal_slots,
                internal_types,
                score_sources,
                wildcard_hidden: wildcard_hidden.into_iter().collect(),
                binding_only: SchemaMap::default(),
                ..SchemaBuildMetadata::default()
            },
        ))
    }

    pub fn lookup_aliases(&self) -> Vec<(&ColumnIdentity, Option<usize>)> {
        let mut aliases = self
            .index
            .aliases
            .iter()
            .map(|(identity, slot)| (identity, (*slot != NULL_SLOT).then_some(*slot)))
            .collect::<Vec<_>>();
        aliases.sort_unstable_by_key(|(identity, _)| *identity);
        aliases
    }

    pub fn lookup_aliases_with_types(
        &self,
    ) -> Vec<(&ColumnIdentity, Option<usize>, Option<&ColumnType>)> {
        self.lookup_aliases()
            .into_iter()
            .map(|(identity, slot)| {
                (
                    identity,
                    slot,
                    self.index
                        .cold
                        .aliases
                        .get(identity)
                        .and_then(Option::as_ref),
                )
            })
            .collect()
    }

    pub fn internal_columns_with_types(
        &self,
    ) -> Vec<(
        crate::ast::InternalColumnRef,
        Option<usize>,
        Option<&ColumnType>,
    )> {
        let mut columns = self
            .index
            .executor_attributes
            .iter()
            .map(|(column, slot)| {
                (
                    *column,
                    (*slot != NULL_SLOT).then_some(*slot),
                    self.index
                        .cold
                        .executor_attribute_types
                        .get(column)
                        .and_then(Option::as_ref),
                )
            })
            .collect::<Vec<_>>();
        columns.sort_unstable_by_key(|(column, _, _)| *column);
        columns
    }

    pub fn score_sources(
        &self,
    ) -> impl Iterator<Item = (Option<&str>, crate::ast::InternalColumnRef)> {
        self.index
            .cold
            .score_sources
            .iter()
            .map(|source| (source.qualifier.as_deref(), source.column))
    }

    pub fn wildcard_hidden_positions(&self) -> impl Iterator<Item = usize> + '_ {
        self.index.cold.wildcard_hidden.iter().copied()
    }

    /// Select logical input positions and attach hidden lookup identities without copying their physical values. Existing hidden aliases are retained, so nested join qualification survives another remap.
    pub fn remap_positions(
        input: &Self,
        columns: &[(String, usize)],
        aliases: &[(ColumnIdentity, usize)],
    ) -> Self {
        let columns = columns
            .iter()
            .map(|(name, logical)| (name.clone(), *logical, input.column_type(*logical).cloned()))
            .collect::<Vec<_>>();
        Self::remap_typed_positions(input, &columns, aliases)
    }

    /// Select logical positions with explicit output types. This is used when a binder inserts an implicit coercion and the output identity no longer has the input slot's declared type.
    pub fn remap_typed_positions(
        input: &Self,
        columns: &[(String, usize, Option<ColumnType>)],
        aliases: &[(ColumnIdentity, usize)],
    ) -> Self {
        let mut collations = input.index.cold.collations.clone();
        collations.clear_columns();
        for (position, (_, source, ty)) in columns.iter().enumerate() {
            collations.column(
                position,
                input.inherited_column_collation(*source),
                ty.as_ref(),
            );
        }
        let output_names = columns
            .iter()
            .map(|(output, _, _)| output.clone())
            .collect::<Vec<_>>();
        let slots = columns
            .iter()
            .map(|(_, logical, _)| input.slot(*logical).unwrap_or(NULL_SLOT))
            .collect();
        let types = columns.iter().map(|(_, _, ty)| ty.clone()).collect();
        let identities = output_names
            .iter()
            .cloned()
            .map(ColumnIdentity::unqualified)
            .collect();
        let wildcard_hidden = columns
            .iter()
            .enumerate()
            .filter_map(|(output, (_, logical, _))| {
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
            collations.alias(
                identity,
                input.inherited_column_collation(*logical),
                input.column_type(*logical),
            );
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
                collations,
                record_fields: input.index.cold.record_fields.clone(),
                aliases: lookup_aliases,
                alias_types,
                internal: input.index.executor_attributes.clone(),
                internal_types: input.index.cold.executor_attribute_types.clone(),
                score_sources: input.index.cold.score_sources.clone(),
                wildcard_hidden,
                binding_only: input.index.cold.binding_only.clone(),
                row_types: input.index.cold.row_types.clone(),
                open_qualifiers: input.index.cold.open_qualifiers.clone(),
                ..SchemaBuildMetadata::default()
            },
        )
    }
}

#[cfg(test)]
mod capture_tests {
    use super::*;
    use crate::ast::InternalRelationId;
    use std::sync::Arc;

    #[test]
    fn output_capture_keeps_positional_metadata_without_sql_aliases() {
        let fields: crate::schema::RecordFields = Arc::from([Some(ColumnType::SmallInteger)]);
        let input = RowSchema::with_qualified_types(
            "source",
            vec!["unused".into(), "text".into(), "record".into()],
            vec![
                Some(ColumnType::Integer),
                Some(ColumnType::Text),
                Some(ColumnType::Record),
            ],
        )
        .with_column_collations([None, Some(950), None])
        .with_record_fields([(2, Arc::clone(&fields))]);
        let selected =
            RowSchema::remap_positions(&input, &[("same".into(), 2), ("same".into(), 1)], &[]);
        let relation = InternalRelationId::allocate();
        let captured = RowSchema::capture_output(&selected, relation);
        assert!(captured.is_empty());
        assert_eq!(captured.physical_width(), 3);
        assert_eq!(captured.internal_slot(relation.column(0)), Some(2));
        assert_eq!(captured.internal_slot(relation.column(1)), Some(1));
        assert_eq!(
            captured.internal_type(relation.column(0)),
            Some(&ColumnType::Record)
        );
        assert_eq!(captured.internal_collation(relation.column(1)), Some(950));
        assert!(Arc::ptr_eq(
            captured.physical_record_fields(2).unwrap(),
            &fields
        ));
        assert_eq!(captured.qualified_slot("source", "record"), None);
        assert_eq!(captured.position("same"), None);
        let other = InternalRelationId::allocate();
        let second = RowSchema::capture_output(&selected, other);
        let joined = RowSchema::join(&captured, &second, []);
        assert_eq!(joined.internal_slot(relation.column(0)), Some(2));
        assert_eq!(joined.internal_slot(other.column(0)), Some(5));
    }

    #[test]
    fn output_capture_assigns_a_slot_to_implicit_null_values() {
        let input = RowSchema::new(vec!["present".into()]);
        let selected = RowSchema::select(&input, &[("missing".into(), "absent".into())]);
        let relation = InternalRelationId::allocate();
        let captured = RowSchema::capture_output(&selected, relation);
        assert_eq!(captured.internal_slot(relation.column(0)), Some(1));
        assert_eq!(captured.physical_width(), 2);
        assert!(captured.is_empty());
    }
}
