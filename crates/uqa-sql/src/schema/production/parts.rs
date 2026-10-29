//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    indexes,
    lease::{boxed_slice, SchemaMemory},
};
use crate::{
    schema::{
        ColumnIdentity, ColumnType, RowSchema, SchemaBuildMetadata, SchemaColdMetadata, SchemaIndex,
    },
    SQLError,
};
use std::sync::Arc;
use uqa_core::memory::{Produced, ProductionControl};

pub(super) fn produce(
    columns: Produced<Vec<String>>,
    identities: Produced<Vec<ColumnIdentity>>,
    types: Produced<Vec<Option<ColumnType>>>,
    slots: Produced<Vec<usize>>,
    physical_width: usize,
    metadata: SchemaBuildMetadata,
    control: &ProductionControl<'_>,
) -> Result<RowSchema, SQLError> {
    control.check()?;
    debug_assert_eq!(columns.len(), identities.len());
    debug_assert_eq!(columns.len(), types.len());
    debug_assert_eq!(columns.len(), slots.len());
    let indexes = indexes::produce(&columns, &identities, control)?;
    let identity_layout = physical_width == columns.len()
        && slots
            .iter()
            .enumerate()
            .all(|(position, slot)| position == *slot);
    let columns = boxed_slice(columns, control)?;
    let identities = boxed_slice(identities, control)?;
    let types = boxed_slice(types, control)?;
    let slots = boxed_slice(slots, control)?;
    let index_memory = control.reserve(size_of::<SchemaIndex>())?;
    let cold_memory = control.reserve(size_of::<SchemaColdMetadata>())?;
    let (columns, columns_memory) = columns.into_parts();
    let (identities, identities_memory) = identities.into_parts();
    let (types, types_memory) = types.into_parts();
    let (slots, slots_memory) = slots.into_parts();
    let memory = control.combine(
        control.combine(index_memory, cold_memory),
        control.combine(
            control.combine(columns_memory, identities_memory),
            control.combine(types_memory, slots_memory),
        ),
    );
    let SchemaBuildMetadata {
        collations,
        row_types,
        row_presence,
        record_fields,
        aliases,
        alias_types,
        internal,
        internal_types,
        score_sources,
        wildcard_hidden,
        binding_only,
        open_qualifiers,
        exact_unqualified_precedence,
        extra_ambiguous_unqualified,
        extra_ambiguous_qualified,
        local_width,
        lateral_restrictions,
    } = metadata;
    debug_assert!(!exact_unqualified_precedence);
    debug_assert!(extra_ambiguous_unqualified.is_empty());
    debug_assert!(extra_ambiguous_qualified.is_empty());
    let index = SchemaIndex {
        columns,
        identities,
        slots,
        physical_width,
        exact: indexes.exact,
        unqualified: indexes.unqualified,
        qualified: indexes.qualified,
        aliases,
        executor_attributes: internal,
        ambiguous_unqualified: indexes.ambiguous_unqualified,
        ambiguous_qualified: indexes.ambiguous_qualified,
        cold: Box::new(SchemaColdMetadata {
            collations,
            row_types,
            row_presence,
            record_fields,
            columns: types,
            aliases: alias_types,
            executor_attribute_types: internal_types,
            score_sources,
            wildcard_hidden,
            binding_only,
            open_qualifiers,
            identity_layout,
            local_width,
            lateral_restrictions,
        }),
        _memory: SchemaMemory::new(memory),
    };
    let schema = RowSchema {
        index: Arc::new(index),
    };
    control.check()?;
    Ok(schema)
}
