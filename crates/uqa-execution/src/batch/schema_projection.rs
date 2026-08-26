//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Projection, canonicalization, and position remapping for row schemas.

use super::{
    ColumnIdentity, ColumnType, ExecError, ExecResult, HashMap, PhysicalLayout, ProjectedSlot,
    RowSchema, SchemaBuildMetadata, ScoreSource, NULL_SLOT,
};

impl RowSchema {
    /// Remove selected executor-only identities after their consumer has run.
    /// The physical fragments remain shareable until the next canonical
    /// boundary, where now-unreferenced slots are naturally discarded.
    pub fn without_internal_attributes(
        input: &Self,
        columns: &[uqa_sql::ast::InternalColumnRef],
    ) -> Self {
        let mut internal = input.index.internal.clone();
        let mut internal_types = input.index.cold.internal.clone();
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
                aliases: input.index.aliases.clone(),
                alias_types: input.index.cold.aliases.clone(),
                internal,
                internal_types,
                score_sources,
                wildcard_hidden: input.index.cold.wildcard_hidden.clone(),
                binding_only: input.index.cold.binding_only.clone(),
                ..SchemaBuildMetadata::default()
            },
        )
    }

    /// Select and optionally rename logical columns while retaining the
    /// child's physical fragments.
    pub fn select(input: &Self, columns: &[(String, String)]) -> Self {
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
                aliases: HashMap::new(),
                alias_types: HashMap::new(),
                internal: input.index.internal.clone(),
                internal_types: input.index.cold.internal.clone(),
                score_sources: input.index.cold.score_sources.clone(),
                binding_only: HashMap::new(),
                ..SchemaBuildMetadata::default()
            },
        )
    }

    /// Build a scalar-projection schema without rebuilding direct input values. A non-pass-through projection hides child identities logically while retaining their physical fragments; an appending projection preserves the child schema and replaces duplicate labels with `PostgreSQL` map-insertion semantics.
    pub(crate) fn project_with_sources(
        input: &Self,
        projected: Vec<(String, Option<ColumnType>, ProjectedSlot)>,
        projected_internal: Vec<(
            uqa_sql::ast::InternalColumnRef,
            Option<ColumnType>,
            ProjectedSlot,
        )>,
        computed_count: usize,
        pass_through: bool,
    ) -> Self {
        let resolve_slot = |source: ProjectedSlot| match source {
            ProjectedSlot::Input(slot) => slot.unwrap_or(NULL_SLOT),
            ProjectedSlot::Computed(position) => input.physical_width() + position,
        };
        let physical_width = input.physical_width() + computed_count;
        let mut internal = input.index.internal.clone();
        let mut internal_types = input.index.cold.internal.clone();
        for (column, ty, source) in projected_internal {
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
                    types[position] = ty;
                    wildcard_hidden.remove(&position);
                } else {
                    identities.push(ColumnIdentity::unqualified(name.clone()));
                    columns.push(name);
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
                    aliases: input.index.aliases.clone(),
                    alias_types: input.index.cold.aliases.clone(),
                    internal,
                    internal_types,
                    score_sources: input.index.cold.score_sources.clone(),
                    wildcard_hidden,
                    binding_only: input.index.cold.binding_only.clone(),
                    ..SchemaBuildMetadata::default()
                },
            );
        }

        let mut columns = Vec::with_capacity(projected.len());
        let mut identities = Vec::with_capacity(projected.len());
        let mut types = Vec::with_capacity(projected.len());
        let mut slots = Vec::with_capacity(projected.len());
        for (name, ty, source) in projected {
            slots.push(resolve_slot(source));
            identities.push(ColumnIdentity::unqualified(name.clone()));
            columns.push(name);
            types.push(ty);
        }
        Self::from_typed_parts_with_aliases_and_exact_precedence(
            columns,
            identities,
            types,
            slots,
            physical_width,
            SchemaBuildMetadata {
                aliases: HashMap::new(),
                alias_types: HashMap::new(),
                internal,
                internal_types,
                score_sources: input.index.cold.score_sources.clone(),
                binding_only: HashMap::new(),
                ..SchemaBuildMetadata::default()
            },
        )
    }

    /// Build a compact positional layout for a blocking or spill boundary.
    /// Logical columns and hidden lookup aliases are remapped to a deduplicated
    /// list of referenced physical slots; projecting a row through the returned
    /// slot list shares its existing value fragments without cloning values.
    pub(crate) fn canonical_projection(&self) -> (Self, Vec<usize>) {
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
        source_aliases.sort_unstable_by(|(left, _), (right, _)| left.cmp(right));
        let aliases = source_aliases
            .into_iter()
            .map(|(name, slot)| {
                (
                    name.clone(),
                    remap_slot(*slot, &mut source_slots, &mut positions),
                )
            })
            .collect();
        let mut source_internal = self.index.internal.iter().collect::<Vec<_>>();
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
                    aliases,
                    alias_types: self.index.cold.aliases.clone(),
                    internal,
                    internal_types: self.index.cold.internal.clone(),
                    score_sources: self.index.cold.score_sources.clone(),
                    wildcard_hidden: self.index.cold.wildcard_hidden.clone(),
                    binding_only: self.index.cold.binding_only.clone(),
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
    pub(crate) fn from_physical_layout(layout: PhysicalLayout) -> ExecResult<Self> {
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
            return Err(ExecError::Other(format!(
                "physical schema has {} columns but {} logical slots",
                columns.len(),
                slots.len()
            )));
        }
        if columns.len() != types.len() {
            return Err(ExecError::Other(format!(
                "physical schema has {} columns but {} logical types",
                columns.len(),
                types.len()
            )));
        }
        if columns.len() != identities.len() {
            return Err(ExecError::Other(format!(
                "physical schema has {} columns but {} logical identities",
                columns.len(),
                identities.len()
            )));
        }
        if wildcard_hidden
            .iter()
            .any(|position| *position >= columns.len())
        {
            return Err(ExecError::Other(
                "physical schema wildcard-hidden position is outside logical width".into(),
            ));
        }
        let slots = slots
            .into_iter()
            .map(|slot| match slot {
                Some(slot) if slot < physical_width => Ok(slot),
                Some(slot) => Err(ExecError::Other(format!(
                    "physical schema logical slot {slot} is outside width {physical_width}"
                ))),
                None => Ok(NULL_SLOT),
            })
            .collect::<ExecResult<Vec<_>>>()?;
        let mut lookup_aliases = HashMap::with_capacity(aliases.len());
        let mut alias_types = HashMap::with_capacity(aliases.len());
        for (identity, slot, ty) in aliases {
            let slot = match slot {
                Some(slot) if slot < physical_width => slot,
                Some(slot) => {
                    return Err(ExecError::Other(format!(
                        "physical schema alias `{identity:?}` slot {slot} is outside width {physical_width}"
                    )))
                }
                None => NULL_SLOT,
            };
            if lookup_aliases.insert(identity.clone(), slot).is_some() {
                return Err(ExecError::Other(format!(
                    "physical schema contains duplicate alias `{identity:?}`"
                )));
            }
            alias_types.insert(identity, ty);
        }
        let mut internal_slots = HashMap::with_capacity(internal.len());
        let mut internal_types = HashMap::with_capacity(internal.len());
        for (column, slot, ty) in internal {
            let slot = match slot {
                Some(slot) if slot < physical_width => slot,
                Some(slot) => {
                    return Err(ExecError::Other(format!(
                        "physical schema internal attribute `{column:?}` slot {slot} is outside width {physical_width}"
                    )))
                }
                None => NULL_SLOT,
            };
            if internal_slots.insert(column, slot).is_some() {
                return Err(ExecError::Other(format!(
                    "physical schema contains duplicate internal attribute `{column:?}`"
                )));
            }
            internal_types.insert(column, ty);
        }
        let score_sources = score_sources
            .into_iter()
            .map(|(qualifier, column)| {
                if !internal_slots.contains_key(&column) {
                    return Err(ExecError::Other(format!(
                        "physical schema score source references missing internal attribute `{column:?}`"
                    )));
                }
                Ok(ScoreSource {
                    qualifier: qualifier.map(Box::<str>::from),
                    column,
                })
            })
            .collect::<ExecResult<Vec<_>>>()?;
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
                wildcard_hidden,
                binding_only: HashMap::new(),
                ..SchemaBuildMetadata::default()
            },
        ))
    }

    pub(crate) fn lookup_aliases(&self) -> Vec<(&ColumnIdentity, Option<usize>)> {
        let mut aliases = self
            .index
            .aliases
            .iter()
            .map(|(identity, slot)| (identity, (*slot != NULL_SLOT).then_some(*slot)))
            .collect::<Vec<_>>();
        aliases.sort_unstable_by_key(|(identity, _)| *identity);
        aliases
    }

    pub(crate) fn lookup_aliases_with_types(
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

    pub(crate) fn internal_columns_with_types(
        &self,
    ) -> Vec<(
        uqa_sql::ast::InternalColumnRef,
        Option<usize>,
        Option<&ColumnType>,
    )> {
        let mut columns = self
            .index
            .internal
            .iter()
            .map(|(column, slot)| {
                (
                    *column,
                    (*slot != NULL_SLOT).then_some(*slot),
                    self.index
                        .cold
                        .internal
                        .get(column)
                        .and_then(Option::as_ref),
                )
            })
            .collect::<Vec<_>>();
        columns.sort_unstable_by_key(|(column, _, _)| *column);
        columns
    }

    pub(crate) fn score_sources(
        &self,
    ) -> impl Iterator<Item = (Option<&str>, uqa_sql::ast::InternalColumnRef)> {
        self.index
            .cold
            .score_sources
            .iter()
            .map(|source| (source.qualifier.as_deref(), source.column))
    }

    pub(crate) fn wildcard_hidden_positions(&self) -> impl Iterator<Item = usize> + '_ {
        self.index.cold.wildcard_hidden.iter().copied()
    }

    /// Select logical input positions and attach hidden lookup identities without copying their physical values. Existing hidden aliases are retained, so nested join qualification survives another remap.
    pub(crate) fn remap_positions(
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
    pub(crate) fn remap_typed_positions(
        input: &Self,
        columns: &[(String, usize, Option<ColumnType>)],
        aliases: &[(ColumnIdentity, usize)],
    ) -> Self {
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
}
