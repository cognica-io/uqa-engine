//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Construction and indexing of immutable row schemas.

use super::{
    Arc, ColumnIdentity, ColumnType, HashMap, HashSet, RowSchema, SchemaBuildMetadata,
    SchemaColdMetadata, SchemaIndex, NULL_SLOT,
};
use uqa_sql::ast::InternalRelationId;

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
        Self::with_identities(columns, identities, types)
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
        let internal = (0..types.len())
            .map(|position| (relation.column(position), position))
            .collect();
        let internal_types = types
            .iter()
            .enumerate()
            .map(|(position, ty)| (relation.column(position), ty.clone()))
            .collect();
        Self::from_typed_parts_with_aliases_and_exact_precedence(
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            types.len(),
            SchemaBuildMetadata {
                internal,
                internal_types,
                ..SchemaBuildMetadata::default()
            },
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
        Self::from_parts_with_aliases(columns, identities, slots, physical_width, HashMap::new())
    }

    fn from_parts_with_aliases(
        columns: Vec<String>,
        identities: Vec<ColumnIdentity>,
        slots: Vec<usize>,
        physical_width: usize,
        aliases: HashMap<ColumnIdentity, usize>,
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
        let SchemaBuildMetadata {
            aliases,
            alias_types,
            internal,
            internal_types,
            score_sources,
            wildcard_hidden,
            binding_only,
            exact_unqualified_precedence,
            extra_ambiguous_unqualified,
            extra_ambiguous_qualified,
        } = metadata;
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
        let mut exact = HashMap::with_capacity(columns.len());
        let mut unqualified = HashMap::with_capacity(columns.len());
        let mut qualified = HashMap::with_capacity(columns.len());
        let mut unqualified_counts: HashMap<Box<str>, usize> = HashMap::new();
        let mut qualified_counts: HashMap<ColumnIdentity, usize> = HashMap::new();

        for (logical, (name, identity)) in columns.iter().zip(&identities).enumerate() {
            // Later writes to the same named field replace the value in a
            // ResultRow. Schema transforms preserve that contract.
            exact.insert(Box::<str>::from(name.as_str()), logical);
            *unqualified_counts
                .entry(identity.column.clone())
                .or_default() += 1;
            unqualified.insert(identity.column.clone(), logical);
            if identity.qualifier.is_some() {
                *qualified_counts.entry(identity.clone()).or_default() += 1;
                qualified.insert(identity.clone(), logical);
            }
        }
        for slot in aliases.values() {
            debug_assert!(*slot == NULL_SLOT || *slot < physical_width);
        }
        for slot in internal.values() {
            debug_assert!(*slot == NULL_SLOT || *slot < physical_width);
        }
        let mut ambiguous_unqualified: HashSet<Box<str>> = unqualified_counts
            .into_iter()
            .filter_map(|(column, count)| {
                (count > 1
                    && !(exact_unqualified_precedence
                        && identities.iter().any(|identity| {
                            identity.qualifier.is_none() && identity.column == column
                        })))
                .then_some(column)
            })
            .collect();
        ambiguous_unqualified.extend(extra_ambiguous_unqualified);
        let mut ambiguous_qualified = qualified_counts
            .into_iter()
            .filter_map(|(identity, count)| (count > 1).then_some(identity))
            .collect::<HashSet<_>>();
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
                    columns: types.into_boxed_slice(),
                    aliases: alias_types,
                    executor_attribute_types: internal_types,
                    score_sources,
                    wildcard_hidden,
                    binding_only,
                    identity_layout,
                }),
            }),
        }
    }
}
