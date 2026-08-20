//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Schema-bound, allocation-light physical rows and batches.
//!
//! Column names belong to [`RowSchema`], not to every row. A physical row is
//! made from shared value fragments. Joins concatenate fragment handles while
//! schemas remap `(qualifier, column)` identities to physical slots; neither
//! operation rebuilds a string-keyed map or clones the contained values.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use smallvec::SmallVec;
use uqa_core::Value;
use uqa_sql::ast::ColumnType;
use uqa_sql::expr::RowLookup;
use uqa_sql::ResultRow;

use crate::physical::{ExecError, ExecResult};

mod materialization;
mod outer_scope;
mod owned_row;

pub use owned_row::OwnedPhysicalRow;

/// Default rows-per-batch hint.
pub const DEFAULT_BATCH_SIZE: usize = 1024;

const NULL_SLOT: usize = usize::MAX;
/// Keep the optional row-lock lineage pointer inside the pre-lineage 64-bit row footprint while retaining seven allocation-free join/projection fragments.
const INLINE_ROW_FRAGMENTS: usize = 7;
static NULL_VALUE: Value = Value::Null;

/// Structured SQL column identity. A qualifier is metadata, never a prefix encoded into the column name, so quoted names containing `.` remain intact.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ColumnIdentity {
    qualifier: Option<Box<str>>,
    column: Box<str>,
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
    identity_layout: bool,
}

#[derive(Default)]
struct SchemaBuildMetadata {
    aliases: HashMap<ColumnIdentity, usize>,
    alias_types: HashMap<ColumnIdentity, Option<ColumnType>>,
    exact_unqualified_precedence: bool,
    extra_ambiguous_unqualified: HashSet<Box<str>>,
    extra_ambiguous_qualified: HashSet<ColumnIdentity>,
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
pub(crate) enum ProjectedSlot {
    Input(Option<usize>),
    Computed,
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

    fn from_typed_parts_with_aliases(
        columns: Vec<String>,
        identities: Vec<ColumnIdentity>,
        types: Vec<Option<ColumnType>>,
        slots: Vec<usize>,
        physical_width: usize,
        aliases: HashMap<ColumnIdentity, usize>,
        alias_types: HashMap<ColumnIdentity, Option<ColumnType>>,
    ) -> Self {
        Self::from_typed_parts_with_aliases_and_exact_precedence(
            columns,
            identities,
            types,
            slots,
            physical_width,
            SchemaBuildMetadata {
                aliases,
                alias_types,
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

    fn from_typed_parts_with_aliases_and_exact_precedence(
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
            exact_unqualified_precedence,
            extra_ambiguous_unqualified,
            extra_ambiguous_qualified,
        } = metadata;
        debug_assert_eq!(columns.len(), slots.len());
        debug_assert_eq!(columns.len(), identities.len());
        debug_assert_eq!(columns.len(), types.len());
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
                ambiguous_unqualified,
                ambiguous_qualified,
                cold: Box::new(SchemaColdMetadata {
                    columns: types.into_boxed_slice(),
                    aliases: alias_types,
                    identity_layout,
                }),
            }),
        }
    }

    pub fn columns(&self) -> &[String] {
        &self.index.columns
    }

    pub fn len(&self) -> usize {
        self.index.columns.len()
    }

    pub fn is_empty(&self) -> bool {
        self.index.columns.is_empty()
    }

    pub fn iter(&self) -> std::slice::Iter<'_, String> {
        self.index.columns.iter()
    }

    /// Static SQL type at one logical output position.
    pub fn column_type(&self, logical: usize) -> Option<&ColumnType> {
        self.index
            .cold
            .columns
            .get(logical)
            .and_then(Option::as_ref)
    }

    /// Static SQL types aligned with [`Self::columns`].
    pub fn column_types(&self) -> &[Option<ColumnType>] {
        &self.index.cold.columns
    }

    /// Structured SQL identities aligned with [`Self::columns`].
    pub fn identities(&self) -> &[ColumnIdentity] {
        &self.index.identities
    }

    /// Whether a visible or hidden lookup identity belongs to `qualifier`.
    #[must_use]
    pub fn has_qualifier(&self, qualifier: &str) -> bool {
        self.index
            .identities
            .iter()
            .chain(self.index.aliases.keys())
            .any(|identity| identity.qualifier() == Some(qualifier))
    }

    pub fn identity(&self, logical: usize) -> Option<&ColumnIdentity> {
        self.index.identities.get(logical)
    }

    pub fn public_name(&self, logical: usize) -> Option<&str> {
        self.identity(logical).map(ColumnIdentity::column)
    }

    pub fn position(&self, name: &str) -> Option<usize> {
        self.index.exact.get(name).copied()
    }

    /// Resolve one visible unqualified SQL identity to its logical position. Ambiguous names deliberately do not select an arbitrary owner.
    pub fn unqualified_position(&self, column: &str) -> Option<usize> {
        if self.index.ambiguous_unqualified.contains(column) {
            return None;
        }
        self.index.unqualified.get(column).copied()
    }

    /// Resolve one visible qualified identity to its logical position.
    pub fn qualified_position(&self, qualifier: &str, column: &str) -> Option<usize> {
        let identity = ColumnIdentity::qualified(qualifier, column);
        if self.index.ambiguous_qualified.contains(&identity) {
            return None;
        }
        self.index.qualified.get(&identity).copied()
    }

    pub fn physical_width(&self) -> usize {
        self.index.physical_width
    }

    /// Resolve one logical output position to its flattened physical slot.
    pub fn physical_slot(&self, logical: usize) -> Option<usize> {
        self.slot(logical)
    }

    /// Resolve a structured SQL identity, including a hidden JOIN USING alias, to its flattened physical slot. Ambiguous identities deliberately return `None` rather than selecting an arbitrary source value.
    pub fn physical_slot_for_identity(&self, identity: &ColumnIdentity) -> Option<usize> {
        match identity.qualifier() {
            Some(qualifier) => self.qualified_slot(qualifier, identity.column()),
            None => self.column_slot(identity.column()),
        }
    }

    pub(crate) fn slot(&self, logical: usize) -> Option<usize> {
        self.index
            .slots
            .get(logical)
            .copied()
            .filter(|slot| *slot != NULL_SLOT)
    }

    fn exact_slot(&self, name: &str) -> Option<usize> {
        self.index
            .exact
            .get(name)
            .and_then(|logical| self.slot(*logical))
            .filter(|slot| *slot != NULL_SLOT)
    }

    fn exact_type(&self, name: &str) -> Option<&ColumnType> {
        self.index
            .exact
            .get(name)
            .and_then(|logical| self.column_type(*logical))
    }

    fn column_slot(&self, name: &str) -> Option<usize> {
        if self.index.ambiguous_unqualified.contains(name) {
            return None;
        }
        self.index
            .unqualified
            .get(name)
            .and_then(|logical| self.slot(*logical))
            .or_else(|| {
                self.index
                    .aliases
                    .get(&ColumnIdentity::unqualified(name))
                    .copied()
            })
            .filter(|slot| *slot != NULL_SLOT)
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
    }

    /// Physical projection layout for `qualifier.*` in relation-column order. Hidden identities introduced by `JOIN ... USING` remain selectable, so each side's wildcard retains its own merged-column value.
    pub fn qualified_star_layout(
        &self,
        qualifier: &str,
    ) -> Vec<(String, usize, Option<ColumnType>)> {
        self.qualified_star_position_layout(qualifier)
            .into_iter()
            .map(|(column, _, slot, ty)| (column, slot, ty))
            .collect()
    }

    /// Bound layout for `qualifier.*`. Visible columns retain their logical positions; hidden aliases such as the suppressed side of `JOIN ... USING` expose only their physical slot.
    pub fn qualified_star_position_layout(
        &self,
        qualifier: &str,
    ) -> Vec<(String, Option<usize>, usize, Option<ColumnType>)> {
        let mut entries = Vec::new();
        let mut visible_layout = HashSet::new();
        for (logical, identity) in self.identities().iter().enumerate() {
            if identity.qualifier() == Some(qualifier) {
                let slot = self.slot(logical).unwrap_or(NULL_SLOT);
                visible_layout.insert((identity.clone(), slot));
                entries.push((
                    identity.clone(),
                    Some(logical),
                    slot,
                    self.column_type(logical).cloned(),
                ));
            }
        }
        for (identity, slot) in &self.index.aliases {
            if identity.qualifier() == Some(qualifier)
                && !visible_layout.contains(&(identity.clone(), *slot))
            {
                entries.push((
                    identity.clone(),
                    None,
                    *slot,
                    self.index.cold.aliases.get(identity).cloned().flatten(),
                ));
            }
        }
        entries.sort_by_key(|(_, _, slot, _)| *slot);
        entries
            .into_iter()
            .map(|(identity, logical, slot, ty)| (identity.column().to_string(), logical, slot, ty))
            .collect()
    }

    pub fn column_is_ambiguous(&self, name: &str) -> bool {
        self.index.ambiguous_unqualified.contains(name)
    }

    pub fn qualified_column_is_ambiguous(&self, qualifier: &str, column: &str) -> bool {
        self.index
            .ambiguous_qualified
            .contains(&ColumnIdentity::qualified(qualifier, column))
    }

    fn qualified_slot(&self, qualifier: &str, column: &str) -> Option<usize> {
        let identity = ColumnIdentity::qualified(qualifier, column);
        if self.index.ambiguous_qualified.contains(&identity) {
            return None;
        }
        self.index
            .qualified
            .get(&identity)
            .and_then(|logical| self.slot(*logical))
            .or_else(|| self.index.aliases.get(&identity).copied())
            .filter(|slot| *slot != NULL_SLOT)
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
        Self::from_typed_parts_with_aliases(
            output_names,
            identities,
            types,
            slots,
            input.physical_width(),
            HashMap::new(),
            HashMap::new(),
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
            return Self::from_typed_parts_with_aliases(
                columns,
                identities,
                types,
                slots,
                next_computed_slot,
                input.index.aliases.clone(),
                input.index.cold.aliases.clone(),
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
        Self::from_typed_parts_with_aliases(
            columns,
            identities,
            types,
            slots,
            next_computed_slot,
            HashMap::new(),
            HashMap::new(),
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
            Self::from_typed_parts_with_aliases(
                self.columns().to_vec(),
                self.identities().to_vec(),
                self.column_types().to_vec(),
                slots,
                source_slots.len(),
                aliases,
                self.index.cold.aliases.clone(),
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
        Ok(Self::from_typed_parts_with_aliases(
            columns,
            identities,
            types,
            slots,
            physical_width,
            lookup_aliases,
            alias_types,
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

    /// Add hidden structured lookup identities for existing logical positions.
    pub fn with_identity_aliases(input: &Self, aliases: &[(ColumnIdentity, usize)]) -> Self {
        let mut lookup_aliases = input.index.aliases.clone();
        let mut alias_types = input.index.cold.aliases.clone();
        for (identity, logical) in aliases {
            lookup_aliases.insert(identity.clone(), input.slot(*logical).unwrap_or(NULL_SLOT));
            alias_types.insert(identity.clone(), input.column_type(*logical).cloned());
        }
        Self::from_typed_parts_with_aliases(
            input.columns().to_vec(),
            input.identities().to_vec(),
            input.column_types().to_vec(),
            input.index.slots.to_vec(),
            input.physical_width(),
            lookup_aliases,
            alias_types,
        )
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
        Self::from_typed_parts_with_aliases(
            output_names,
            identities,
            types,
            slots,
            input.physical_width(),
            lookup_aliases,
            alias_types,
        )
    }

    /// Select logical positions with explicit public labels, SQL identities, and types while preserving hidden aliases and physical fragments.
    pub(crate) fn remap_typed_identities(
        input: &Self,
        columns: &[(String, ColumnIdentity, usize, Option<ColumnType>)],
        aliases: &[(ColumnIdentity, usize)],
    ) -> Self {
        let output_names = columns
            .iter()
            .map(|(output, _, _, _)| output.clone())
            .collect();
        let identities = columns
            .iter()
            .map(|(_, identity, _, _)| identity.clone())
            .collect();
        let slots = columns
            .iter()
            .map(|(_, _, logical, _)| input.slot(*logical).unwrap_or(NULL_SLOT))
            .collect();
        let types = columns.iter().map(|(_, _, _, ty)| ty.clone()).collect();
        let mut lookup_aliases = input.index.aliases.clone();
        let mut alias_types = input.index.cold.aliases.clone();
        for (identity, logical) in aliases {
            lookup_aliases.insert(identity.clone(), input.slot(*logical).unwrap_or(NULL_SLOT));
            alias_types.insert(identity.clone(), input.column_type(*logical).cloned());
        }
        Self::from_typed_parts_with_aliases(
            output_names,
            identities,
            types,
            slots,
            input.physical_width(),
            lookup_aliases,
            alias_types,
        )
    }

    /// Append freshly-computed values to an existing physical row. Reusing an
    /// existing output name replaces its logical slot just like map insertion.
    pub fn append(input: &Self, names: &[String]) -> Self {
        let columns = names
            .iter()
            .cloned()
            .map(|name| (name, None))
            .collect::<Vec<_>>();
        Self::append_typed(input, &columns)
    }

    /// Append freshly computed values with static SQL output types.
    pub fn append_typed(input: &Self, names: &[(String, Option<ColumnType>)]) -> Self {
        let mut columns = input.columns().to_vec();
        let mut identities = input.identities().to_vec();
        let mut types = input.column_types().to_vec();
        let mut slots = input.index.slots.to_vec();
        let base = input.physical_width();
        for (offset, (name, ty)) in names.iter().enumerate() {
            let slot = base + offset;
            if let Some(position) = columns.iter().position(|column| column == name) {
                slots[position] = slot;
                identities[position] = ColumnIdentity::unqualified(name);
                types[position].clone_from(ty);
            } else {
                columns.push(name.clone());
                identities.push(ColumnIdentity::unqualified(name));
                types.push(ty.clone());
                slots.push(slot);
            }
        }
        Self::from_typed_parts_with_aliases(
            columns,
            identities,
            types,
            slots,
            base + names.len(),
            input.index.aliases.clone(),
            input.index.cold.aliases.clone(),
        )
    }

    /// Compose two child layouts while retaining duplicate logical labels.
    /// Qualified and positional resolution can then distinguish both input
    /// slots without copying either value fragment.
    pub fn join(
        left: &Self,
        right: &Self,
        extra_columns: impl IntoIterator<Item = String>,
    ) -> Self {
        let mut columns = left.columns().to_vec();
        let mut identities = left.identities().to_vec();
        let mut types = left.column_types().to_vec();
        let mut slots = left.index.slots.to_vec();
        let right_base = left.physical_width();
        let mut aliases = left.index.aliases.clone();
        let mut alias_types = left.index.cold.aliases.clone();
        aliases.extend(right.index.aliases.iter().map(|(name, slot)| {
            (
                name.clone(),
                if *slot == NULL_SLOT {
                    NULL_SLOT
                } else {
                    right_base + *slot
                },
            )
        }));
        alias_types.extend(
            right
                .index
                .cold
                .aliases
                .iter()
                .map(|(name, ty)| (name.clone(), ty.clone())),
        );
        for (right_logical, column) in right.columns().iter().enumerate() {
            let slot = right
                .slot(right_logical)
                .map_or(NULL_SLOT, |slot| right_base + slot);
            columns.push(column.clone());
            identities.push(right.identities()[right_logical].clone());
            types.push(right.column_type(right_logical).cloned());
            slots.push(slot);
        }
        for column in extra_columns {
            if !columns.contains(&column) {
                identities.push(ColumnIdentity::unqualified(column.clone()));
                columns.push(column);
                types.push(None);
                slots.push(NULL_SLOT);
            }
        }
        Self::from_typed_parts_with_aliases(
            columns,
            identities,
            types,
            slots,
            left.physical_width() + right.physical_width(),
            aliases,
            alias_types,
        )
    }

    pub fn view<'a>(&'a self, row: &'a PhysicalRow) -> PhysicalRowView<'a> {
        PhysicalRowView { schema: self, row }
    }

    /// Re-express a row emitted under this schema in `target`'s complete physical layout without cloning any values. Visible columns are matched by logical position; hidden lookup aliases are matched by their structured identity. This is used when two equivalent operator pipelines expose the same logical row through different physical slot arrangements.
    pub fn relayout_physical_row(
        &self,
        row: PhysicalRow,
        target: &Self,
    ) -> ExecResult<PhysicalRow> {
        if self.len() != target.len() {
            return Err(ExecError::Other(format!(
                "cannot relayout {} logical columns as {} logical columns",
                self.len(),
                target.len()
            )));
        }

        let mut source_slots = vec![None; target.physical_width()];
        let mut assign = |target_slot: usize, source_slot: usize| -> ExecResult<()> {
            if target_slot == NULL_SLOT {
                return Ok(());
            }
            match source_slots[target_slot] {
                Some(existing) if existing != source_slot => Err(ExecError::Other(format!(
                    "physical relayout maps target slot {target_slot} to both source slots {existing} and {source_slot}"
                ))),
                Some(_) => Ok(()),
                None => {
                    source_slots[target_slot] = Some(source_slot);
                    Ok(())
                }
            }
        };

        for logical in 0..target.len() {
            assign(target.index.slots[logical], self.index.slots[logical])?;
        }

        for (identity, target_slot) in &target.index.aliases {
            if *target_slot == NULL_SLOT {
                continue;
            }
            let mut matching_slots = self
                .index
                .identities
                .iter()
                .enumerate()
                .filter_map(|(logical, candidate)| {
                    (candidate == identity).then_some(self.index.slots[logical])
                })
                .chain(self.index.aliases.get(identity).copied())
                .collect::<Vec<_>>();
            matching_slots.sort_unstable();
            matching_slots.dedup();
            let source_slot = match matching_slots.as_slice() {
                [source_slot] => *source_slot,
                [] => {
                    return Err(ExecError::Other(format!(
                        "physical relayout source is missing lookup identity `{identity:?}`"
                    )))
                }
                _ => {
                    return Err(ExecError::Other(format!(
                        "physical relayout source has ambiguous lookup identity `{identity:?}`"
                    )))
                }
            };
            assign(*target_slot, source_slot)?;
        }

        let source_slots = source_slots
            .into_iter()
            .map(|slot| slot.unwrap_or(NULL_SLOT))
            .collect::<Vec<_>>();
        Ok(row.project_slots(&source_slots))
    }
}

#[derive(Debug, Clone, PartialEq)]
struct RowFragment {
    values: Arc<Vec<Value>>,
    /// Fragment-local output slot -> stored value slot. `None` is the common
    /// contiguous case. A projection lets an in-memory scan share its stored
    /// row even when column pruning selects or reorders fields.
    projection: Option<Arc<[usize]>>,
}

impl RowFragment {
    fn contiguous(values: Arc<Vec<Value>>) -> Self {
        Self {
            values,
            projection: None,
        }
    }

    fn projected(values: Arc<Vec<Value>>, projection: Arc<[usize]>) -> Self {
        debug_assert!(projection
            .iter()
            .all(|slot| *slot == NULL_SLOT || *slot < values.len()));
        let identity = projection.len() == values.len()
            && projection
                .iter()
                .enumerate()
                .all(|(index, slot)| index == *slot);
        if identity {
            Self::contiguous(values)
        } else {
            Self {
                values,
                projection: Some(projection),
            }
        }
    }

    fn len(&self) -> usize {
        self.projection
            .as_ref()
            .map_or(self.values.len(), |projection| projection.len())
    }

    fn get(&self, slot: usize) -> Option<&Value> {
        match self.projection.as_ref() {
            Some(projection) => match projection.get(slot).copied()? {
                NULL_SLOT => Some(&NULL_VALUE),
                stored => self.values.get(stored),
            },
            None => self.values.get(slot),
        }
    }

    fn stored_slot(&self, slot: usize) -> Option<usize> {
        match self.projection.as_ref() {
            Some(projection) => projection.get(slot).copied(),
            None => (slot < self.values.len()).then_some(slot),
        }
    }

    fn into_prefix(mut self, width: usize) -> Self {
        debug_assert!(width <= self.len());
        if width == self.len() {
            return self;
        }
        if let Some(projection) = self.projection.as_ref() {
            self.projection = Some(Arc::from(&projection[..width]));
            return self;
        }
        if let Some(values) = Arc::get_mut(&mut self.values) {
            values.truncate(width);
        } else {
            self.projection = Some((0..width).collect::<Arc<[usize]>>());
        }
        self
    }
}

type RowFragments = SmallVec<[RowFragment; INLINE_ROW_FRAGMENTS]>;

/// A physical row owns no column names. Each fragment is created by a scan or
/// projection and shared thereafter; joining rows copies only `Arc` handles.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PhysicalRow {
    fragments: RowFragments,
    lock_origins: Option<Arc<Vec<RowLockOrigin>>>,
}

/// One output position in a mixed physical projection.
#[derive(Debug, Clone, PartialEq)]
pub enum RowProjectionValue {
    /// Reuse one flattened slot from the input row.
    InputSlot(usize),
    /// Append a newly computed value.
    Owned(Value),
}

impl PhysicalRow {
    pub fn from_values(values: Vec<Value>) -> Self {
        let mut fragments = RowFragments::new();
        if !values.is_empty() {
            fragments.push(RowFragment::contiguous(Arc::new(values)));
        }
        Self {
            fragments,
            lock_origins: None,
        }
    }

    /// Build a row by sharing a stored positional value vector and applying a
    /// fragment-local slot projection. Neither the values nor contained
    /// strings are cloned.
    pub fn from_shared_values(values: Arc<Vec<Value>>, projection: Arc<[usize]>) -> Self {
        let mut fragments = RowFragments::new();
        if !projection.is_empty() {
            fragments.push(RowFragment::projected(values, projection));
        }
        Self {
            fragments,
            lock_origins: None,
        }
    }

    pub fn from_result_row(schema: &RowSchema, mut row: ResultRow) -> Self {
        let values = schema
            .columns()
            .iter()
            .map(|column| row.remove(column).unwrap_or(Value::Null))
            .collect();
        Self::from_values(values)
    }

    pub fn nulls(width: usize) -> Self {
        Self::from_values(vec![Value::Null; width])
    }

    pub fn append_values(mut self, values: Vec<Value>) -> Self {
        if !values.is_empty() {
            self.fragments
                .push(RowFragment::contiguous(Arc::new(values)));
        }
        self
    }

    pub fn concat(left: &Self, right: &Self) -> Self {
        let mut fragments =
            RowFragments::with_capacity(left.fragments.len() + right.fragments.len());
        fragments.extend(left.fragments.iter().cloned());
        fragments.extend(right.fragments.iter().cloned());
        let lock_origins =
            concat_lock_origins(left.lock_origins.as_ref(), right.lock_origins.as_ref());
        Self {
            fragments,
            lock_origins,
        }
    }

    pub fn concat_left_owned(mut left: Self, right: &Self) -> Self {
        left.fragments.extend(right.fragments.iter().cloned());
        left.lock_origins =
            concat_lock_origins(left.lock_origins.as_ref(), right.lock_origins.as_ref());
        left
    }

    pub fn concat_right_owned(left: &Self, mut right: Self) -> Self {
        let mut fragments =
            RowFragments::with_capacity(left.fragments.len() + right.fragments.len());
        fragments.extend(left.fragments.iter().cloned());
        fragments.append(&mut right.fragments);
        let lock_origins =
            concat_lock_origins(left.lock_origins.as_ref(), right.lock_origins.as_ref());
        Self {
            fragments,
            lock_origins,
        }
    }

    pub(crate) fn value(&self, mut slot: usize) -> Option<&Value> {
        for fragment in &self.fragments {
            if slot < fragment.len() {
                return fragment.get(slot);
            }
            slot -= fragment.len();
        }
        None
    }

    /// Re-express selected flattened slots as a compact positional row while
    /// sharing the underlying value vectors. Consecutive slots backed by the
    /// same source fragment share one projection fragment; no `Value` (and in
    /// particular no string payload) is cloned.
    pub(crate) fn project_slots(&self, slots: &[usize]) -> Self {
        let mut output = RowFragments::new();
        let null_values = Arc::new(Vec::new());
        let null_source = self.fragments.len();
        let mut current_source = None;
        let mut current_values: Option<Arc<Vec<Value>>> = None;
        let mut current_projection = Vec::new();

        let flush = |output: &mut RowFragments,
                     values: &mut Option<Arc<Vec<Value>>>,
                     projection: &mut Vec<usize>| {
            if let Some(values) = values.take() {
                output.push(RowFragment::projected(
                    values,
                    Arc::from(std::mem::take(projection)),
                ));
            }
        };

        for requested in slots {
            let mut remaining = *requested;
            let resolved = if remaining == NULL_SLOT {
                None
            } else {
                let mut found = None;
                for (fragment_index, fragment) in self.fragments.iter().enumerate() {
                    if remaining < fragment.len() {
                        found = fragment
                            .stored_slot(remaining)
                            .filter(|slot| *slot != NULL_SLOT)
                            .map(|stored| (fragment_index, Arc::clone(&fragment.values), stored));
                        break;
                    }
                    remaining -= fragment.len();
                }
                found
            };
            let (source, values, stored) = resolved.map_or_else(
                || (null_source, Arc::clone(&null_values), NULL_SLOT),
                |(source, values, stored)| (source, values, stored),
            );
            if current_source != Some(source) {
                flush(&mut output, &mut current_values, &mut current_projection);
                current_source = Some(source);
                current_values = Some(values);
            }
            current_projection.push(stored);
        }
        flush(&mut output, &mut current_values, &mut current_projection);
        Self {
            fragments: output,
            lock_origins: self.lock_origins.clone(),
        }
    }

    /// Build an output row from shared input slots and newly computed values while preserving their requested order and sharing row metadata.
    pub fn project_with_values(
        &self,
        values: impl IntoIterator<Item = RowProjectionValue>,
    ) -> Self {
        fn flush_slots(source: &PhysicalRow, output: &mut RowFragments, slots: &mut Vec<usize>) {
            if slots.is_empty() {
                return;
            }
            let mut projected = source.project_slots(slots);
            output.append(&mut projected.fragments);
            slots.clear();
        }

        fn flush_owned(output: &mut RowFragments, owned: &mut Vec<Value>) {
            if owned.is_empty() {
                return;
            }
            output.push(RowFragment::contiguous(Arc::new(std::mem::take(owned))));
        }

        let mut fragments = RowFragments::new();
        let mut slots = Vec::new();
        let mut owned = Vec::new();
        for value in values {
            match value {
                RowProjectionValue::InputSlot(slot) => {
                    flush_owned(&mut fragments, &mut owned);
                    slots.push(slot);
                }
                RowProjectionValue::Owned(value) => {
                    flush_slots(self, &mut fragments, &mut slots);
                    owned.push(value);
                }
            }
        }
        flush_slots(self, &mut fragments, &mut slots);
        flush_owned(&mut fragments, &mut owned);
        Self {
            fragments,
            lock_origins: self.lock_origins.clone(),
        }
    }

    pub fn fragment_count(&self) -> usize {
        self.fragments.len()
    }

    pub(crate) fn into_prefix(self, width: usize) -> Self {
        let mut remaining = width;
        let mut fragments = RowFragments::new();
        for fragment in self.fragments {
            if remaining == 0 {
                break;
            }
            let fragment_width = fragment.len();
            if fragment_width <= remaining {
                fragments.push(fragment);
                remaining -= fragment_width;
            } else {
                fragments.push(fragment.into_prefix(remaining));
                remaining = 0;
            }
        }
        debug_assert_eq!(remaining, 0, "physical row prefix exceeds row width");
        Self {
            fragments,
            lock_origins: self.lock_origins,
        }
    }
}

mod physical_row_view;
pub use physical_row_view::PhysicalRowView;

mod batches;
pub use batches::Batch;

mod row_lock_origins;
use row_lock_origins::concat_lock_origins;
pub use row_lock_origins::RowLockOrigin;

#[cfg(test)]
mod tests;
