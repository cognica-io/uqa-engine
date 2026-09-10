//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL column identities, declared types, and logical slot layouts.

use crate::ast::{ColumnType, InternalColumnRef};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

mod name_binding;
mod open_columns;
mod outer_scope;
mod schema_composition;
mod schema_construction;
mod schema_layout;
mod schema_projection;
mod schema_remap;

const NULL_SLOT: usize = usize::MAX;

/// Invalid schema layout supplied by a planner or a persisted row layout.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct SchemaLayoutError(pub String);

type SchemaLayoutResult<T> = Result<T, SchemaLayoutError>;

/// Structured SQL column identity. A qualifier is metadata, never a prefix encoded into the column name, so quoted names containing `.` remain intact.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
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

#[derive(Debug, PartialEq, Eq)]
struct SchemaIndex {
    /// Public/materialized output labels in logical order.
    columns: Box<[String]>,
    /// SQL lookup identities aligned with `columns`.
    identities: Box<[ColumnIdentity]>,
    /// Logical column position -> flattened physical value position.
    slots: Box<[usize]>,
    physical_width: usize,
    /// Structural lookup by physical/public label. SQL name binding uses `unqualified` or `qualified`, never this map.
    exact: HashMap<Box<str>, usize>,
    unqualified: HashMap<Box<str>, usize>,
    qualified: HashMap<ColumnIdentity, usize>,
    /// Additional lookup identities that point directly at an existing physical slot without becoming output columns. Correlated table aliases use this to expose `(alias, column)` without duplicating the value.
    aliases: HashMap<ColumnIdentity, usize>,
    /// Executor-only relation/attribute identities mapped directly to physical
    /// slots. These never participate in SQL name lookup or wildcard output.
    executor_attributes: HashMap<InternalColumnRef, usize>,
    /// Visible unqualified names with more than one logical owner.
    ambiguous_unqualified: HashSet<Box<str>>,
    /// Visible qualified identities with more than one logical owner.
    ambiguous_qualified: HashSet<ColumnIdentity>,
    /// Static type metadata stays behind a cold pointer so declared SQL identities do not enlarge or displace the cache-hot row lookup fields above.
    cold: Box<SchemaColdMetadata>,
}

#[derive(Debug, PartialEq, Eq)]
struct SchemaColdMetadata {
    /// `None` is an as-yet unresolved type, not a runtime NULL value.
    columns: Box<[Option<ColumnType>]>,
    aliases: HashMap<ColumnIdentity, Option<ColumnType>>,
    executor_attribute_types: HashMap<InternalColumnRef, Option<ColumnType>>,
    score_sources: Vec<ScoreSource>,
    /// Logical attributes omitted from unqualified and qualified wildcard
    /// expansion. Explicit references and projections remain ordinary SQL
    /// columns; only the source-owned metadata positions are hidden.
    wildcard_hidden: HashSet<usize>,
    /// Static name-binding identities with no runtime slot. Unlike aliases,
    /// these are never part of qualified wildcard expansion or spill layout.
    binding_only: HashMap<ColumnIdentity, Option<ColumnType>>,
    /// Names supplied only when a document or native-function source opens.
    open_qualifiers: HashSet<Option<Box<str>>>,
    identity_layout: bool,
}

#[derive(Default)]
struct SchemaBuildMetadata {
    aliases: HashMap<ColumnIdentity, usize>,
    alias_types: HashMap<ColumnIdentity, Option<ColumnType>>,
    internal: HashMap<InternalColumnRef, usize>,
    internal_types: HashMap<InternalColumnRef, Option<ColumnType>>,
    score_sources: Vec<ScoreSource>,
    wildcard_hidden: HashSet<usize>,
    binding_only: HashMap<ColumnIdentity, Option<ColumnType>>,
    open_qualifiers: HashSet<Option<Box<str>>>,
    exact_unqualified_precedence: bool,
    extra_ambiguous_unqualified: HashSet<Box<str>>,
    extra_ambiguous_qualified: HashSet<ColumnIdentity>,
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
    Computed(usize),
}

impl RowSchema {
    /// Logical-to-physical slots; usize::MAX denotes a NULL slot.
    #[doc(hidden)]
    pub fn layout_slots(&self) -> &[usize] {
        &self.index.slots
    }

    #[doc(hidden)]
    pub fn is_identity_layout(&self) -> bool {
        self.index.cold.identity_layout
    }
}

pub mod join_output;
