//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL column identities, declared types, and logical slot layouts.

use crate::ast::{ColumnType, InternalColumnRef};
use std::collections::{HashMap, HashSet};

/// Lookup maps keyed by column names and identities. Name binding constructs and probes these maps for every column of every bound schema, so they hash with `ahash` instead of the standard library's `SipHash` default.
pub(crate) use lookup_map::SchemaMap;
pub(crate) use lookup_set::SchemaSet;
use std::sync::Arc;

mod collations;
mod identity_lookup;
mod input_slots;
mod lateral;
mod lookup_map;
mod lookup_set;
mod name_binding;
mod open_columns;
mod outer_scope;
mod production;
mod record_fields;
mod relation_presence;
mod relation_types;
mod result_columns;
mod schema_composition;
mod schema_construction;
mod schema_layout;
mod schema_projection;
mod schema_remap;
pub use collations::CollationBinding;
use collations::Collations;
pub use input_slots::InputSlots;
pub use record_fields::RecordFields;
pub(crate) use record_fields::{RecordDescriptor, RecordFieldMetadata, RecordMetadata};

const NULL_SLOT: usize = usize::MAX;

/// Invalid schema layout supplied by a planner or a persisted row layout.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct SchemaLayoutError(pub String);

type SchemaLayoutResult<T> = Result<T, SchemaLayoutError>;

/// Structured SQL column identity. A qualifier is metadata, never a prefix encoded into the column name, so quoted names containing `.` remain intact.
#[derive(
    Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub struct ColumnIdentity {
    qualifier: Option<Box<str>>,
    column: Box<str>,
}

/// One score-bearing relation carried through the executor under an opaque internal attribute. The optional qualifier is SQL namespace metadata; the score value itself is never addressed by a magic SQL column name.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ScoreSource {
    qualifier: Option<Box<str>>,
    column: InternalColumnRef,
}

impl ColumnIdentity {
    #[must_use]
    pub fn unqualified(column: impl Into<String>) -> Self {
        Self {
            qualifier: None,
            column: Box::<str>::from(column.into()),
        }
    }

    #[must_use]
    pub fn qualified(qualifier: impl Into<String>, column: impl Into<String>) -> Self {
        Self {
            qualifier: Some(Box::<str>::from(qualifier.into())),
            column: Box::<str>::from(column.into()),
        }
    }

    #[must_use]
    pub fn qualifier(&self) -> Option<&str> {
        self.qualifier.as_deref()
    }

    #[must_use]
    pub fn column(&self) -> &str {
        &self.column
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SchemaIndex {
    /// Public/materialized output labels in logical order.
    columns: Box<[String]>,
    /// SQL lookup identities aligned with `columns`.
    identities: Box<[ColumnIdentity]>,
    /// Logical column position -> flattened physical value position.
    slots: Box<[usize]>,
    physical_width: usize,
    /// Structural lookup by physical/public label. SQL name binding uses `unqualified` or `qualified`, never this map.
    exact: SchemaMap<Box<str>, usize>,
    unqualified: SchemaMap<Box<str>, usize>,
    qualified: SchemaMap<ColumnIdentity, usize>,
    /// Additional lookup identities that point directly at an existing physical slot without becoming output columns. Correlated table aliases use this to expose `(alias, column)` without duplicating the value.
    aliases: SchemaMap<ColumnIdentity, usize>,
    /// Executor-only relation/attribute identities mapped directly to physical
    /// slots. These never participate in SQL name lookup or wildcard output.
    executor_attributes: SchemaMap<InternalColumnRef, usize>,
    /// Visible unqualified names with more than one logical owner.
    ambiguous_unqualified: SchemaSet<Box<str>>,
    /// Visible qualified identities with more than one logical owner.
    ambiguous_qualified: SchemaSet<ColumnIdentity>,
    /// Static type metadata stays behind a cold pointer so declared SQL identities do not enlarge or displace the cache-hot row lookup fields above.
    cold: Box<SchemaColdMetadata>,
    _memory: production::lease::SchemaMemory,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SchemaColdMetadata {
    collations: Collations,
    row_types: relation_types::RelationRowTypes,
    row_presence: relation_presence::RelationPresence,
    record_fields: SchemaMap<usize, RecordMetadata>,
    /// `None` is an as-yet unresolved type, not a runtime NULL value.
    columns: Box<[Option<ColumnType>]>,
    aliases: SchemaMap<ColumnIdentity, Option<ColumnType>>,
    executor_attribute_types: SchemaMap<InternalColumnRef, Option<ColumnType>>,
    score_sources: Vec<ScoreSource>,
    /// Logical attributes omitted from unqualified and qualified wildcard
    /// expansion. Explicit references and projections remain ordinary SQL
    /// columns; only the source-owned metadata positions are hidden.
    wildcard_hidden: SchemaSet<usize>,
    /// Static name-binding identities with no runtime slot. Unlike aliases,
    /// these are never part of qualified wildcard expansion or spill layout.
    binding_only: SchemaMap<ColumnIdentity, Option<ColumnType>>,
    /// Names supplied only when a document or native-function source opens.
    open_qualifiers: SchemaSet<Option<Box<str>>>,
    identity_layout: bool,
    /// The physical width of the schema's own columns when [`RowSchema::with_outer_schema`] overlaid an enclosing query's columns after them; `None` when every column is the schema's own.
    local_width: Option<usize>,
    lateral_restrictions: Option<Arc<lateral::LateralRestrictions>>,
}

#[derive(Default)]
struct SchemaBuildMetadata {
    collations: Collations,
    row_types: relation_types::RelationRowTypes,
    row_presence: relation_presence::RelationPresence,
    record_fields: SchemaMap<usize, RecordMetadata>,
    aliases: SchemaMap<ColumnIdentity, usize>,
    alias_types: SchemaMap<ColumnIdentity, Option<ColumnType>>,
    internal: SchemaMap<InternalColumnRef, usize>,
    internal_types: SchemaMap<InternalColumnRef, Option<ColumnType>>,
    score_sources: Vec<ScoreSource>,
    wildcard_hidden: SchemaSet<usize>,
    binding_only: SchemaMap<ColumnIdentity, Option<ColumnType>>,
    open_qualifiers: SchemaSet<Option<Box<str>>>,
    exact_unqualified_precedence: bool,
    extra_ambiguous_unqualified: SchemaSet<Box<str>>,
    extra_ambiguous_qualified: SchemaSet<ColumnIdentity>,
    local_width: Option<usize>,
    lateral_restrictions: Option<Arc<lateral::LateralRestrictions>>,
}

pub struct PhysicalLayout {
    pub columns: Vec<String>,
    pub identities: Vec<ColumnIdentity>,
    pub types: Vec<Option<ColumnType>>,
    pub slots: Vec<Option<usize>>,
    pub physical_width: usize,
    pub aliases: Vec<(ColumnIdentity, Option<usize>, Option<ColumnType>)>,
    pub internal: Vec<(InternalColumnRef, Option<usize>, Option<ColumnType>)>,
    pub score_sources: Vec<(Option<String>, InternalColumnRef)>,
    pub wildcard_hidden: HashSet<usize>,
}

/// Immutable column layout shared by an operator and all of its batches.
///
/// `columns` are the logical output labels. `slots` may point into a wider
/// composite physical row after a projection/rename, allowing those operators
/// to change row shape without moving any values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowSchema {
    index: Arc<SchemaIndex>,
}

/// Physical source of one scalar-projection output. Direct input slots stay in the child row; only computed values extend its physical layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectedSlot {
    Input(Option<usize>),
    /// A SQL binding supplies its own collation even when another logical column shares this physical slot or the slot is NULL.
    BoundInput {
        physical: Option<usize>,
        collation: Option<u32>,
    },
    Computed {
        position: usize,
        collation: Option<u32>,
    },
}

impl RowSchema {
    /// Logical-to-physical slots; usize::MAX denotes a NULL slot.
    #[doc(hidden)]
    pub fn layout_slots(&self) -> &[usize] {
        &self.index.slots
    }

    /// Whether `other` is a clone of this schema, sharing the index that defines its names and layout.
    #[must_use]
    pub fn shares_index(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.index, &other.index)
    }

    #[doc(hidden)]
    pub fn is_identity_layout(&self) -> bool {
        self.index.cold.identity_layout
    }
}

pub mod join_output;

mod declarations;
pub mod expressions;
pub use declarations::{SchemaBindingContext, SchemaExpressionCatalog};
pub mod generated;
pub mod indexes;

pub mod columns;
pub mod composites;
pub mod constraints;
pub mod defaults;
pub mod domains;

pub mod dependencies;

pub mod check_inheritance;

pub mod table_creation;

pub mod inheritance;

pub mod foreign_keys;

pub mod referenced_partitions;

pub mod sequences;

pub mod constraint_metadata;

pub mod keys;

pub mod constraint_changes;

pub mod constraint_views;

pub mod table_alteration;

pub mod removal;

pub mod namespaces;
pub mod relation_alteration;
pub mod view_creation;

pub mod truncate;

pub mod type_objects;

pub mod foreign_servers;
pub mod foreign_tables;
pub mod foreign_wrappers;

pub mod retention;

mod type_lookup;
pub use type_lookup::{ColumnTypeSchema, ScalarTypeSchema};

impl uqa_core::memory::ObserveAllocations for ColumnIdentity {
    fn observe_allocations<'a>(
        &'a self,
        observer: &mut uqa_core::memory::AllocationObserver<'a, '_>,
    ) -> Result<(), uqa_core::ValueRetentionError> {
        let Self { qualifier, column } = self;
        observer.child(qualifier)?;
        observer.child(column)?;
        Ok(())
    }
}
