//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Construction and indexing of immutable row schemas.

use super::{
    Arc, ColumnIdentity, ColumnType, RowSchema, SchemaBuildMetadata, SchemaColdMetadata,
    SchemaIndex, NULL_SLOT,
};
use crate::ast::InternalRelationId;
use crate::schema::{SchemaMap, SchemaSet};

#[cfg(test)]
thread_local! {
    static SCHEMA_BUILDS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(super) fn count_builds<T>(build: impl FnOnce() -> T) -> (T, usize) {
    let before = SCHEMA_BUILDS.get();
    let result = build();
    (result, SCHEMA_BUILDS.get() - before)
}

impl Default for RowSchema {
    fn default() -> Self {
        Self::new(Vec::new())
    }
}

impl From<Vec<String>> for RowSchema {
    fn from(columns: Vec<String>) -> Self {
        Self::new(columns)
    }
}

impl RowSchema {
    pub fn new(columns: Vec<String>) -> Self {
        let width = columns.len();
        let identities = columns
            .iter()
            .cloned()
            .map(ColumnIdentity::unqualified)
            .collect();
        Self::from_parts(columns, identities, (0..width).collect(), width)
    }

    /// Build a positional schema with statically bound SQL types.
    pub fn with_types(columns: Vec<String>, types: Vec<Option<ColumnType>>) -> Self {
        let width = columns.len();
        assert_eq!(width, types.len(), "row schema column/type width mismatch");
        let identities = columns
            .iter()
            .cloned()
            .map(ColumnIdentity::unqualified)
            .collect();
        Self::from_typed_parts_with_aliases_and_exact_precedence(
            columns,
            identities,
            types,
            (0..width).collect(),
            width,
            SchemaBuildMetadata::default(),
        )
    }

    /// Build a positional schema whose visible columns all belong to one relation qualifier while retaining their public names verbatim.
    pub fn with_qualified_types(
        qualifier: &str,
        columns: Vec<String>,
        types: Vec<Option<ColumnType>>,
    ) -> Self {
        let identities = columns
            .iter()
            .cloned()
            .map(|column| ColumnIdentity::qualified(qualifier, column))
            .collect();
        let mut schema = Self::with_identities(columns, identities, types);
        if schema.is_empty() {
            Arc::make_mut(&mut schema.index).cold.row_types =
                super::relation_types::RelationRowTypes::anonymous(qualifier);
        }
        schema
    }

    /// Build a positional source from its catalog declarations without discarding selected column collations. Attribute numbers remain a catalog identity, independent of these query positions.
    pub fn with_column_definitions(
        qualifier: Option<&str>,
        columns: &[crate::ast::ColumnDef],
    ) -> Self {
        Self::with_column_metadata(
            qualifier,
            columns.iter().map(|column| {
                (
                    column.name.clone(),
                    Some(column.ty.clone()),
                    column.collation_oid,
                )
            }),
        )
    }

    /// Build a positional source while selecting names, types and collations together. This accepts projected, aliased and system columns without a second declaration lookup or a dense collation buffer.
    pub fn with_column_metadata(
        qualifier: Option<&str>,
        columns: impl IntoIterator<Item = (String, Option<ColumnType>, Option<u32>)>,
    ) -> Self {
        let columns = columns.into_iter();
        let capacity = columns.size_hint().0;
        let mut names = Vec::with_capacity(capacity);
        let mut identities = Vec::with_capacity(capacity);
        let mut types = Vec::with_capacity(capacity);
        let mut metadata = SchemaBuildMetadata::default();
        for (name, ty, oid) in columns {
            assert!(
                oid.is_none_or(|oid| oid == 0
                    || ty
                        .as_ref()
                        .is_some_and(super::composites::type_is_collatable)),
                "collation requires a collatable column type"
            );
            metadata.collations.column(names.len(), oid, ty.as_ref());
            identities.push(qualifier.map_or_else(
                || ColumnIdentity::unqualified(&name),
                |qualifier| ColumnIdentity::qualified(qualifier, &name),
            ));
            names.push(name);
            types.push(ty);
        }
        let width = names.len();
        if let Some(qualifier) = qualifier.filter(|_| width == 0) {
            metadata.row_types = super::relation_types::RelationRowTypes::anonymous(qualifier);
        }
        Self::from_typed_parts_with_aliases_and_exact_precedence(
            names,
            identities,
            types,
            (0..width).collect(),
            width,
            metadata,
        )
    }

    /// Build a positional schema from explicit structured identities.
    pub fn with_identities(
        columns: Vec<String>,
        identities: Vec<ColumnIdentity>,
        types: Vec<Option<ColumnType>>,
    ) -> Self {
        let width = columns.len();
        assert_eq!(
            width,
            identities.len(),
            "row schema column/identity width mismatch"
        );
        assert_eq!(width, types.len(), "row schema column/type width mismatch");
        Self::from_typed_parts_with_aliases_and_exact_precedence(
            columns,
            identities,
            types,
            (0..width).collect(),
            width,
            SchemaBuildMetadata::default(),
        )
    }

    /// Build a physical row layout whose values are addressable only through
    /// an opaque internal relation identity. It contributes no SQL-visible
    /// columns and therefore cannot affect `*` expansion or name binding.
    pub fn with_internal_relation_types(
        relation: InternalRelationId,
        types: Vec<Option<ColumnType>>,
    ) -> Self {
        Self::with_internal_relation_metadata(relation, types.into_iter().map(|ty| (ty, None)))
    }

    /// Build executor-only inputs with the selected collation of each logical attribute, including NULL values.
    pub fn with_internal_relation_metadata(
        relation: InternalRelationId,
        columns: impl IntoIterator<Item = (Option<ColumnType>, Option<u32>)>,
    ) -> Self {
        let mut metadata = SchemaBuildMetadata::default();
        let mut width = 0;
        for (position, (ty, oid)) in columns.into_iter().enumerate() {
            assert!(
                oid.is_none_or(|oid| oid == 0
                    || ty
                        .as_ref()
                        .is_some_and(super::composites::type_is_collatable)),
                "collation requires a collatable column type"
            );
            let column = relation.column(position);
            metadata.collations.internal(column, oid, ty.as_ref());
            metadata.internal.insert(column, position);
            metadata.internal_types.insert(column, ty);
            width += 1;
        }
        Self::from_typed_parts_with_aliases_and_exact_precedence(
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            width,
            metadata,
        )
    }

    /// Build the lookup semantics of a named compatibility row. An exact bare
    /// key in a map is authoritative even when qualified metadata keys share
    /// its suffix; physical relational schemas continue to treat multiple
    /// visible owners as ambiguous.
    pub fn from_named_columns(columns: Vec<String>) -> Self {
        let width = columns.len();
        let identities = columns
            .iter()
            .cloned()
            .map(ColumnIdentity::unqualified)
            .collect();
        Self::from_parts_with_aliases_and_exact_precedence(
            columns,
            identities,
            (0..width).collect(),
            width,
            SchemaBuildMetadata {
                exact_unqualified_precedence: true,
                ..SchemaBuildMetadata::default()
            },
        )
    }

    fn from_parts(
        columns: Vec<String>,
        identities: Vec<ColumnIdentity>,
        slots: Vec<usize>,
        physical_width: usize,
    ) -> Self {
        Self::from_parts_with_aliases(
            columns,
            identities,
            slots,
            physical_width,
            SchemaMap::default(),
        )
    }

    fn from_parts_with_aliases(
        columns: Vec<String>,
        identities: Vec<ColumnIdentity>,
        slots: Vec<usize>,
        physical_width: usize,
        aliases: SchemaMap<ColumnIdentity, usize>,
    ) -> Self {
        Self::from_parts_with_aliases_and_exact_precedence(
            columns,
            identities,
            slots,
            physical_width,
            SchemaBuildMetadata {
                aliases,
                ..SchemaBuildMetadata::default()
            },
        )
    }

    fn from_parts_with_aliases_and_exact_precedence(
        columns: Vec<String>,
        identities: Vec<ColumnIdentity>,
        slots: Vec<usize>,
        physical_width: usize,
        metadata: SchemaBuildMetadata,
    ) -> Self {
        let types = vec![None; columns.len()];
        Self::from_typed_parts_with_aliases_and_exact_precedence(
            columns,
            identities,
            types,
            slots,
            physical_width,
            metadata,
        )
    }

    pub(super) fn from_typed_parts_with_aliases_and_exact_precedence(
        columns: Vec<String>,
        identities: Vec<ColumnIdentity>,
        types: Vec<Option<ColumnType>>,
        slots: Vec<usize>,
        physical_width: usize,
        metadata: SchemaBuildMetadata,
    ) -> Self {
        #[cfg(test)]
        SCHEMA_BUILDS.set(SCHEMA_BUILDS.get() + 1);
        let SchemaBuildMetadata {
            mut collations,
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
        collations.retain(&types, &alias_types, &internal_types, &binding_only);
        debug_assert_eq!(columns.len(), slots.len());
        debug_assert_eq!(columns.len(), identities.len());
        debug_assert_eq!(columns.len(), types.len());
        debug_assert!(wildcard_hidden
            .iter()
            .all(|position| *position < columns.len()));
        let identity_layout = physical_width == columns.len()
            && slots
                .iter()
                .enumerate()
                .all(|(position, slot)| position == *slot);
        let hasher = ahash::RandomState::default;
        let mut exact = SchemaMap::with_capacity_and_hasher(columns.len(), hasher());
        let mut unqualified = SchemaMap::with_capacity_and_hasher(columns.len(), hasher());
        let mut qualified = SchemaMap::with_capacity_and_hasher(columns.len(), hasher());
        // A name owned by more than one logical column is ambiguous; the replaced entry of each lookup map records the repetition without a second map per name.
        let mut ambiguous_unqualified = SchemaSet::with_hasher(hasher());
        let mut ambiguous_qualified = SchemaSet::with_hasher(hasher());
        for (logical, (name, identity)) in columns.iter().zip(&identities).enumerate() {
            // Later writes to the same named field replace the value in a
            // ResultRow. Schema transforms preserve that contract.
            exact.insert(Box::<str>::from(name.as_str()), logical);
            if unqualified
                .insert(identity.column.clone(), logical)
                .is_some()
            {
                ambiguous_unqualified.insert(identity.column.clone());
            }
            if identity.qualifier.is_some() && qualified.insert(identity.clone(), logical).is_some()
            {
                ambiguous_qualified.insert(identity.clone());
            }
        }
        for slot in aliases.values() {
            debug_assert!(*slot == NULL_SLOT || *slot < physical_width);
        }
        for slot in internal.values() {
            debug_assert!(*slot == NULL_SLOT || *slot < physical_width);
        }
        if exact_unqualified_precedence {
            ambiguous_unqualified.retain(|column| {
                !identities
                    .iter()
                    .any(|identity| identity.qualifier.is_none() && identity.column == *column)
            });
        }
        ambiguous_unqualified.extend(extra_ambiguous_unqualified);
        ambiguous_qualified.extend(extra_ambiguous_qualified);
        Self {
            index: Arc::new(SchemaIndex {
                columns: columns.into_boxed_slice(),
                identities: identities.into_boxed_slice(),
                slots: slots.into_boxed_slice(),
                physical_width,
                exact,
                unqualified,
                qualified,
                aliases,
                executor_attributes: internal,
                ambiguous_unqualified,
                ambiguous_qualified,
                cold: Box::new(SchemaColdMetadata {
                    collations,
                    row_types,
                    row_presence,
                    record_fields,
                    columns: types.into_boxed_slice(),
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
                _memory: super::production::lease::SchemaMemory::default(),
            }),
        }
    }
}
