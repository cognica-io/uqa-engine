//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Projection, canonicalization, and position remapping for row schemas.

use super::{
    ColumnIdentity, ColumnType, ExecError, ExecResult, HashMap, ProjectedSlot, RowSchema,
    SchemaBuildMetadata, NULL_SLOT,
};

impl RowSchema {
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
                binding_only: HashMap::new(),
                ..SchemaBuildMetadata::default()
            },
        )
    }

    /// Build a scalar-projection schema without rebuilding direct input values. A non-pass-through projection hides child identities logically while retaining their physical fragments; an appending projection preserves the child schema and replaces duplicate labels with `PostgreSQL` map-insertion semantics.
    pub(crate) fn project_with_sources(
        input: &Self,
        projected: Vec<(String, Option<ColumnType>, ProjectedSlot)>,
        pass_through: bool,
    ) -> Self {
        let mut next_computed_slot = input.physical_width();
        let mut resolve_slot = |source: ProjectedSlot| match source {
            ProjectedSlot::Input(slot) => slot.unwrap_or(NULL_SLOT),
            ProjectedSlot::Computed => {
                let slot = next_computed_slot;
                next_computed_slot += 1;
                slot
            }
        };

        if pass_through {
            let mut columns = input.columns().to_vec();
            let mut identities = input.identities().to_vec();
            let mut types = input.column_types().to_vec();
            let mut slots = input.index.slots.to_vec();
            for (name, ty, source) in projected {
                let slot = resolve_slot(source);
                if let Some(position) = columns.iter().position(|column| column == &name) {
                    slots[position] = slot;
                    identities[position] = ColumnIdentity::unqualified(name);
                    types[position] = ty;
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
                next_computed_slot,
                SchemaBuildMetadata {
                    aliases: input.index.aliases.clone(),
                    alias_types: input.index.cold.aliases.clone(),
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
            next_computed_slot,
            SchemaBuildMetadata {
                aliases: HashMap::new(),
                alias_types: HashMap::new(),
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
    pub(crate) fn from_physical_layout(
        columns: Vec<String>,
        identities: Vec<ColumnIdentity>,
        types: Vec<Option<ColumnType>>,
        slots: Vec<Option<usize>>,
        physical_width: usize,
        aliases: Vec<(ColumnIdentity, Option<usize>, Option<ColumnType>)>,
    ) -> ExecResult<Self> {
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
        Ok(Self::from_typed_parts_with_aliases_and_exact_precedence(
            columns,
            identities,
            types,
            slots,
            physical_width,
            SchemaBuildMetadata {
                aliases: lookup_aliases,
                alias_types,
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
}
