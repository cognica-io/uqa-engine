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

/// Default rows-per-batch hint.
pub const DEFAULT_BATCH_SIZE: usize = 1024;

const NULL_SLOT: usize = usize::MAX;
const INLINE_ROW_FRAGMENTS: usize = 8;
static NULL_VALUE: Value = Value::Null;

#[derive(Debug, PartialEq, Eq)]
struct SchemaIndex {
    columns: Box<[String]>,
    /// Logical column position -> flattened physical value position.
    slots: Box<[usize]>,
    physical_width: usize,
    exact: HashMap<Box<str>, usize>,
    qualified: HashMap<Box<str>, HashMap<Box<str>, usize>>,
    suffix: HashMap<Box<str>, usize>,
    qualified_owners: HashSet<Box<str>>,
    /// Additional lookup identities that point directly at an existing
    /// physical slot without becoming output columns. Correlated table aliases
    /// use this to expose `alias.column` without duplicating the value.
    aliases: HashMap<Box<str>, usize>,
    alias_qualified: HashMap<Box<str>, HashMap<Box<str>, usize>>,
    /// Visible unqualified names with more than one logical owner.
    ambiguous_unqualified: HashSet<Box<str>>,
    /// Static type metadata stays behind a cold pointer so declared SQL identities do not enlarge or displace the cache-hot row lookup fields above.
    cold: Box<SchemaColdMetadata>,
}

#[derive(Debug, PartialEq, Eq)]
struct SchemaColdMetadata {
    /// `None` is an as-yet unresolved type, not a runtime NULL value.
    columns: Box<[Option<ColumnType>]>,
    aliases: HashMap<Box<str>, Option<ColumnType>>,
    identity_layout: bool,
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
        Self::from_parts(columns, (0..width).collect(), width)
    }

    /// Build a positional schema with statically bound SQL types.
    pub fn with_types(columns: Vec<String>, types: Vec<Option<ColumnType>>) -> Self {
        let width = columns.len();
        assert_eq!(width, types.len(), "row schema column/type width mismatch");
        Self::from_typed_parts_with_aliases_and_exact_precedence(
            columns,
            types,
            (0..width).collect(),
            width,
            HashMap::new(),
            HashMap::new(),
            false,
            HashSet::new(),
        )
    }

    /// Build the lookup semantics of a named compatibility row. An exact bare
    /// key in a map is authoritative even when qualified metadata keys share
    /// its suffix; physical relational schemas continue to treat multiple
    /// visible owners as ambiguous.
    pub fn from_named_columns(columns: Vec<String>) -> Self {
        let width = columns.len();
        Self::from_parts_with_aliases_and_exact_precedence(
            columns,
            (0..width).collect(),
            width,
            HashMap::new(),
            true,
            HashSet::new(),
        )
    }

    fn from_parts(columns: Vec<String>, slots: Vec<usize>, physical_width: usize) -> Self {
        Self::from_parts_with_aliases(columns, slots, physical_width, HashMap::new())
    }

    fn from_parts_with_aliases(
        columns: Vec<String>,
        slots: Vec<usize>,
        physical_width: usize,
        aliases: HashMap<Box<str>, usize>,
    ) -> Self {
        Self::from_parts_with_aliases_and_exact_precedence(
            columns,
            slots,
            physical_width,
            aliases,
            false,
            HashSet::new(),
        )
    }

    fn from_typed_parts_with_aliases(
        columns: Vec<String>,
        types: Vec<Option<ColumnType>>,
        slots: Vec<usize>,
        physical_width: usize,
        aliases: HashMap<Box<str>, usize>,
        alias_types: HashMap<Box<str>, Option<ColumnType>>,
    ) -> Self {
        Self::from_typed_parts_with_aliases_and_exact_precedence(
            columns,
            types,
            slots,
            physical_width,
            aliases,
            alias_types,
            false,
            HashSet::new(),
        )
    }

    fn from_parts_with_aliases_and_exact_precedence(
        columns: Vec<String>,
        slots: Vec<usize>,
        physical_width: usize,
        aliases: HashMap<Box<str>, usize>,
        exact_unqualified_precedence: bool,
        extra_ambiguous_unqualified: HashSet<Box<str>>,
    ) -> Self {
        let types = vec![None; columns.len()];
        Self::from_typed_parts_with_aliases_and_exact_precedence(
            columns,
            types,
            slots,
            physical_width,
            aliases,
            HashMap::new(),
            exact_unqualified_precedence,
            extra_ambiguous_unqualified,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn from_typed_parts_with_aliases_and_exact_precedence(
        columns: Vec<String>,
        types: Vec<Option<ColumnType>>,
        slots: Vec<usize>,
        physical_width: usize,
        aliases: HashMap<Box<str>, usize>,
        alias_types: HashMap<Box<str>, Option<ColumnType>>,
        exact_unqualified_precedence: bool,
        extra_ambiguous_unqualified: HashSet<Box<str>>,
    ) -> Self {
        debug_assert_eq!(columns.len(), slots.len());
        debug_assert_eq!(columns.len(), types.len());
        let identity_layout = physical_width == columns.len()
            && slots
                .iter()
                .enumerate()
                .all(|(position, slot)| position == *slot);
        let mut exact = HashMap::with_capacity(columns.len());
        let mut qualified: HashMap<Box<str>, HashMap<Box<str>, usize>> = HashMap::new();
        let mut alias_qualified: HashMap<Box<str>, HashMap<Box<str>, usize>> = HashMap::new();
        let mut suffix_candidates: HashMap<Box<str>, (String, usize)> = HashMap::new();
        let mut unqualified_counts: HashMap<Box<str>, usize> = HashMap::new();
        let mut qualified_owners = HashSet::new();

        for (logical, name) in columns.iter().enumerate() {
            // Later writes to the same named field replace the value in a
            // ResultRow. Schema transforms preserve that contract.
            exact.insert(Box::<str>::from(name.as_str()), logical);
            let unqualified = name
                .rsplit_once('.')
                .map_or(name.as_str(), |(_, column)| column);
            *unqualified_counts
                .entry(Box::<str>::from(unqualified))
                .or_default() += 1;
            if let Some((qualifier, column)) = name.rsplit_once('.') {
                qualified
                    .entry(Box::<str>::from(qualifier))
                    .or_default()
                    .insert(Box::<str>::from(column), logical);
                qualified_owners.insert(Box::<str>::from(column));
                match suffix_candidates.get_mut(column) {
                    Some((candidate, slot)) if name < candidate => {
                        candidate.clone_from(name);
                        *slot = logical;
                    }
                    None => {
                        suffix_candidates.insert(Box::<str>::from(column), (name.clone(), logical));
                    }
                    Some(_) => {}
                }
            }
        }
        for (name, slot) in &aliases {
            debug_assert!(*slot == NULL_SLOT || *slot < physical_width);
            if let Some((qualifier, column)) = name.rsplit_once('.') {
                alias_qualified
                    .entry(Box::<str>::from(qualifier))
                    .or_default()
                    .insert(Box::<str>::from(column), *slot);
                qualified_owners.insert(Box::<str>::from(column));
            }
        }
        let suffix = suffix_candidates
            .into_iter()
            .map(|(column, (_, logical))| (column, logical))
            .collect();
        let mut ambiguous_unqualified: HashSet<Box<str>> = unqualified_counts
            .into_iter()
            .filter_map(|(column, count)| {
                (count > 1
                    && !(exact_unqualified_precedence
                        && columns.iter().any(|name| name == column.as_ref())))
                .then_some(column)
            })
            .collect();
        ambiguous_unqualified.extend(extra_ambiguous_unqualified);
        Self {
            index: Arc::new(SchemaIndex {
                columns: columns.into_boxed_slice(),
                slots: slots.into_boxed_slice(),
                physical_width,
                exact,
                qualified,
                suffix,
                qualified_owners,
                aliases,
                alias_qualified,
                ambiguous_unqualified,
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

    pub fn position(&self, name: &str) -> Option<usize> {
        self.index.exact.get(name).copied()
    }

    pub fn physical_width(&self) -> usize {
        self.index.physical_width
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
            .or_else(|| self.index.aliases.get(name).copied())
            .filter(|slot| *slot != NULL_SLOT)
    }

    fn column_slot(&self, name: &str) -> Option<usize> {
        if !name.contains('.') && self.index.ambiguous_unqualified.contains(name) {
            return None;
        }
        self.exact_slot(name).or_else(|| {
            self.index
                .suffix
                .get(name)
                .and_then(|logical| self.slot(*logical))
        })
    }

    /// Resolve an unqualified or exact logical identity to its static type.
    pub fn type_of(&self, name: &str) -> Option<&ColumnType> {
        if !name.contains('.') && self.index.ambiguous_unqualified.contains(name) {
            return None;
        }
        self.index
            .exact
            .get(name)
            .and_then(|logical| self.column_type(*logical))
            .or_else(|| self.index.cold.aliases.get(name).and_then(Option::as_ref))
            .or_else(|| {
                self.index
                    .suffix
                    .get(name)
                    .and_then(|logical| self.column_type(*logical))
            })
    }

    /// Resolve a qualified logical identity to its static type using the same
    /// ownership and shadowing rules as value lookup.
    pub fn qualified_type(&self, qualifier: &str, column: &str, key: &str) -> Option<&ColumnType> {
        let exact = self
            .index
            .qualified
            .get(qualifier)
            .and_then(|columns| columns.get(column))
            .and_then(|logical| self.column_type(*logical))
            .or_else(|| {
                let alias = format!("{qualifier}.{column}");
                self.index
                    .cold
                    .aliases
                    .get(alias.as_str())
                    .and_then(Option::as_ref)
            })
            .or_else(|| (!key.is_empty()).then(|| self.type_of(key)).flatten());
        if exact.is_some() || self.index.qualified_owners.contains(column) {
            return exact;
        }
        self.type_of(column)
    }

    pub fn column_is_ambiguous(&self, name: &str) -> bool {
        !name.contains('.') && self.index.ambiguous_unqualified.contains(name)
    }

    fn qualified_slot(&self, qualifier: &str, column: &str, key: &str) -> Option<usize> {
        let exact = self
            .index
            .qualified
            .get(qualifier)
            .and_then(|columns| columns.get(column))
            .and_then(|logical| self.slot(*logical))
            .or_else(|| {
                self.index
                    .alias_qualified
                    .get(qualifier)
                    .and_then(|columns| columns.get(column))
                    .copied()
            })
            .or_else(|| (!key.is_empty()).then(|| self.exact_slot(key)).flatten());
        if exact.is_some() || self.index.qualified_owners.contains(column) {
            return exact;
        }
        self.exact_slot(column)
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
            .map(|(_, source)| input.type_of(source).cloned())
            .collect();
        Self::from_typed_parts_with_aliases(
            output_names,
            types,
            slots,
            input.physical_width(),
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
        types: Vec<Option<ColumnType>>,
        slots: Vec<Option<usize>>,
        physical_width: usize,
        aliases: Vec<(String, Option<usize>, Option<ColumnType>)>,
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
        for (name, slot, ty) in aliases {
            let slot = match slot {
                Some(slot) if slot < physical_width => slot,
                Some(slot) => {
                    return Err(ExecError::Other(format!(
                    "physical schema alias `{name}` slot {slot} is outside width {physical_width}"
                )))
                }
                None => NULL_SLOT,
            };
            if lookup_aliases
                .insert(Box::<str>::from(name.as_str()), slot)
                .is_some()
            {
                return Err(ExecError::Other(format!(
                    "physical schema contains duplicate alias `{name}`"
                )));
            }
            alias_types.insert(Box::<str>::from(name.as_str()), ty);
        }
        Ok(Self::from_typed_parts_with_aliases(
            columns,
            types,
            slots,
            physical_width,
            lookup_aliases,
            alias_types,
        ))
    }

    pub(crate) fn lookup_aliases(&self) -> Vec<(&str, Option<usize>)> {
        let mut aliases = self
            .index
            .aliases
            .iter()
            .map(|(name, slot)| (name.as_ref(), (*slot != NULL_SLOT).then_some(*slot)))
            .collect::<Vec<_>>();
        aliases.sort_unstable_by_key(|(name, _)| *name);
        aliases
    }

    pub(crate) fn lookup_aliases_with_types(
        &self,
    ) -> Vec<(&str, Option<usize>, Option<&ColumnType>)> {
        self.lookup_aliases()
            .into_iter()
            .map(|(name, slot)| {
                (
                    name,
                    slot,
                    self.index.cold.aliases.get(name).and_then(Option::as_ref),
                )
            })
            .collect()
    }

    /// Add hidden lookup names for existing slots. These aliases participate
    /// in column resolution but not schema iteration or final materialization.
    pub fn with_lookup_aliases(input: &Self, aliases: &[(String, String)]) -> Self {
        let mut lookup_aliases = input.index.aliases.clone();
        let mut alias_types = input.index.cold.aliases.clone();
        for (alias, source) in aliases {
            lookup_aliases.insert(
                Box::<str>::from(alias.as_str()),
                input.exact_slot(source).unwrap_or(NULL_SLOT),
            );
            alias_types.insert(
                Box::<str>::from(alias.as_str()),
                input.type_of(source).cloned(),
            );
        }
        Self::from_typed_parts_with_aliases(
            input.columns().to_vec(),
            input.column_types().to_vec(),
            input.index.slots.to_vec(),
            input.physical_width(),
            lookup_aliases,
            alias_types,
        )
    }

    /// Select logical input positions and attach hidden lookup identities
    /// without copying their physical values. Existing hidden aliases are
    /// retained, so nested join qualification survives another remap.
    pub(crate) fn remap_positions(
        input: &Self,
        columns: &[(String, usize)],
        aliases: &[(String, usize)],
    ) -> Self {
        let columns = columns
            .iter()
            .map(|(name, logical)| (name.clone(), *logical, input.column_type(*logical).cloned()))
            .collect::<Vec<_>>();
        Self::remap_typed_positions(input, &columns, aliases)
    }

    /// Select logical positions with explicit output types. This is used when
    /// a binder inserts an implicit coercion and the output identity no longer
    /// has the input slot's declared type.
    pub(crate) fn remap_typed_positions(
        input: &Self,
        columns: &[(String, usize, Option<ColumnType>)],
        aliases: &[(String, usize)],
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
        let mut lookup_aliases = input.index.aliases.clone();
        let mut alias_types = input.index.cold.aliases.clone();
        for (alias, logical) in aliases {
            lookup_aliases.insert(
                Box::<str>::from(alias.as_str()),
                input.slot(*logical).unwrap_or(NULL_SLOT),
            );
            alias_types.insert(
                Box::<str>::from(alias.as_str()),
                input.column_type(*logical).cloned(),
            );
        }
        Self::from_typed_parts_with_aliases(
            output_names,
            types,
            slots,
            input.physical_width(),
            lookup_aliases,
            alias_types,
        )
    }

    /// Overlay a correlated outer scope without exposing its columns to star expansion. Current-scope names and relation qualifiers shadow outer names, while an ambiguous outer unqualified name remains ambiguous when the current scope has no owner.
    pub(crate) fn with_outer_scope(input: &Self, outer_columns: &[String]) -> Self {
        let columns = outer_columns
            .iter()
            .cloned()
            .map(|column| (column, None))
            .collect::<Vec<_>>();
        Self::with_typed_outer_scope(input, &columns)
    }

    /// Overlay a statically typed correlated outer scope. The outer columns
    /// remain hidden from schema iteration and star expansion, but both
    /// qualified and unqualified lookup aliases retain their declared SQL
    /// types for expression binding.
    pub fn with_typed_outer_scope(
        input: &Self,
        outer_columns: &[(String, Option<ColumnType>)],
    ) -> Self {
        let outer_base = input.physical_width();
        let current_qualifiers = input
            .index
            .qualified
            .keys()
            .chain(input.index.alias_qualified.keys())
            .map(std::convert::AsRef::as_ref)
            .collect::<HashSet<_>>();
        let mut current_unqualified = input
            .columns()
            .iter()
            .map(|column| {
                column
                    .rsplit_once('.')
                    .map_or(column.as_str(), |(_, name)| name)
            })
            .collect::<HashSet<_>>();
        current_unqualified.extend(
            input
                .index
                .aliases
                .keys()
                .filter(|name| !name.contains('.'))
                .map(std::convert::AsRef::as_ref),
        );

        let mut aliases = input.index.aliases.clone();
        let mut alias_types = input.index.cold.aliases.clone();
        let mut outer_exact = HashSet::<&str>::new();
        let mut outer_qualified = HashMap::<&str, Vec<usize>>::new();
        for (position, (name, ty)) in outer_columns.iter().enumerate() {
            let slot = outer_base + position;
            if let Some((qualifier, column)) = name.rsplit_once('.') {
                if !current_qualifiers.contains(qualifier) {
                    aliases.insert(Box::<str>::from(name.as_str()), slot);
                    alias_types.insert(Box::<str>::from(name.as_str()), ty.clone());
                    outer_qualified.entry(column).or_default().push(position);
                }
            } else {
                outer_exact.insert(name);
                if !current_unqualified.contains(name.as_str()) {
                    aliases.insert(Box::<str>::from(name.as_str()), slot);
                    alias_types.insert(Box::<str>::from(name.as_str()), ty.clone());
                }
            }
        }

        let mut outer_ambiguous = HashSet::new();
        for (column, positions) in outer_qualified {
            if current_unqualified.contains(column) || outer_exact.contains(column) {
                continue;
            }
            match positions.as_slice() {
                [position] => {
                    aliases.insert(Box::<str>::from(column), outer_base + position);
                    alias_types
                        .insert(Box::<str>::from(column), outer_columns[*position].1.clone());
                }
                [_, _, ..] => {
                    outer_ambiguous.insert(Box::<str>::from(column));
                }
                [] => {}
            }
        }

        Self::from_typed_parts_with_aliases_and_exact_precedence(
            input.columns().to_vec(),
            input.column_types().to_vec(),
            input.index.slots.to_vec(),
            input.physical_width() + outer_columns.len(),
            aliases,
            alias_types,
            false,
            outer_ambiguous,
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
        let mut types = input.column_types().to_vec();
        let mut slots = input.index.slots.to_vec();
        let base = input.physical_width();
        for (offset, (name, ty)) in names.iter().enumerate() {
            let slot = base + offset;
            if let Some(position) = columns.iter().position(|column| column == name) {
                slots[position] = slot;
                types[position].clone_from(ty);
            } else {
                columns.push(name.clone());
                types.push(ty.clone());
                slots.push(slot);
            }
        }
        Self::from_typed_parts_with_aliases(
            columns,
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
            types.push(right.column_type(right_logical).cloned());
            slots.push(slot);
        }
        for column in extra_columns {
            if !columns.contains(&column) {
                columns.push(column);
                types.push(None);
                slots.push(NULL_SLOT);
            }
        }
        Self::from_typed_parts_with_aliases(
            columns,
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

    fn materialize_result_row(&self, row: PhysicalRow) -> ResultRow {
        if self.index.cold.identity_layout {
            let values = row.into_values();
            debug_assert_eq!(self.len(), values.len());
            return self.columns().iter().cloned().zip(values).collect();
        }
        self.view(&row).to_result_row()
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

    /// Consume this fragment at an explicit row-materialization boundary. Unshared contiguous values, and the common prefix projection emitted by blocking operators, retain their existing allocations instead of being cloned one value at a time.
    fn into_values(self) -> Vec<Value> {
        let Self { values, projection } = self;
        let Some(projection) = projection else {
            return Arc::try_unwrap(values).unwrap_or_else(|values| values.as_ref().clone());
        };
        let mut values = match Arc::try_unwrap(values) {
            Ok(values) => values,
            Err(values) => {
                return projection
                    .iter()
                    .map(|slot| {
                        if *slot == NULL_SLOT {
                            Value::Null
                        } else {
                            values.get(*slot).cloned().unwrap_or(Value::Null)
                        }
                    })
                    .collect();
            }
        };
        if projection
            .iter()
            .enumerate()
            .all(|(position, slot)| position == *slot)
        {
            values.truncate(projection.len());
            return values;
        }

        let mut remaining = vec![0usize; values.len()];
        for slot in projection.iter().copied().filter(|slot| *slot != NULL_SLOT) {
            if let Some(count) = remaining.get_mut(slot) {
                *count += 1;
            }
        }
        let mut values = values.into_iter().map(Some).collect::<Vec<_>>();
        projection
            .iter()
            .map(|slot| {
                if *slot == NULL_SLOT {
                    return Value::Null;
                }
                let Some(count) = remaining.get_mut(*slot) else {
                    return Value::Null;
                };
                *count -= 1;
                if *count == 0 {
                    values[*slot].take().unwrap_or(Value::Null)
                } else {
                    values[*slot].clone().unwrap_or(Value::Null)
                }
            })
            .collect()
    }
}

type RowFragments = SmallVec<[RowFragment; INLINE_ROW_FRAGMENTS]>;

/// A physical row owns no column names. Each fragment is created by a scan or
/// projection and shared thereafter; joining rows copies only `Arc` handles.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PhysicalRow {
    fragments: RowFragments,
}

impl PhysicalRow {
    pub fn from_values(values: Vec<Value>) -> Self {
        let mut fragments = RowFragments::new();
        if !values.is_empty() {
            fragments.push(RowFragment::contiguous(Arc::new(values)));
        }
        Self { fragments }
    }

    /// Build a row by sharing a stored positional value vector and applying a
    /// fragment-local slot projection. Neither the values nor contained
    /// strings are cloned.
    pub fn from_shared_values(values: Arc<Vec<Value>>, projection: Arc<[usize]>) -> Self {
        let mut fragments = RowFragments::new();
        if !projection.is_empty() {
            fragments.push(RowFragment::projected(values, projection));
        }
        Self { fragments }
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
        Self { fragments }
    }

    pub fn concat_left_owned(mut left: Self, right: &Self) -> Self {
        left.fragments.extend(right.fragments.iter().cloned());
        left
    }

    pub fn concat_right_owned(left: &Self, mut right: Self) -> Self {
        let mut fragments =
            RowFragments::with_capacity(left.fragments.len() + right.fragments.len());
        fragments.extend(left.fragments.iter().cloned());
        fragments.append(&mut right.fragments);
        Self { fragments }
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
        Self { fragments: output }
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
        Self { fragments }
    }

    fn into_values(mut self) -> Vec<Value> {
        if self.fragments.len() == 1 {
            return self
                .fragments
                .pop()
                .expect("one physical row fragment exists")
                .into_values();
        }
        let capacity = self.fragments.iter().map(RowFragment::len).sum();
        let mut values = Vec::with_capacity(capacity);
        for fragment in self.fragments {
            values.extend(fragment.into_values());
        }
        values
    }
}

/// Stack-only schema/row pair implementing the scalar evaluator's read seam.
#[derive(Debug, Clone, Copy)]
pub struct PhysicalRowView<'a> {
    schema: &'a RowSchema,
    row: &'a PhysicalRow,
}

impl<'a> PhysicalRowView<'a> {
    pub fn get(&self, name: &str) -> Option<&'a Value> {
        self.schema
            .exact_slot(name)
            .and_then(|slot| self.row.value(slot))
    }

    pub fn value_at(&self, logical: usize) -> Option<&'a Value> {
        self.schema
            .slot(logical)
            .and_then(|slot| self.row.value(slot))
    }

    pub fn iter(&'a self) -> impl Iterator<Item = (&'a str, &'a Value)> + 'a {
        self.schema
            .columns()
            .iter()
            .enumerate()
            .map(|(logical, column)| {
                (
                    column.as_str(),
                    self.value_at(logical).unwrap_or(&NULL_VALUE),
                )
            })
    }

    pub fn to_result_row(&self) -> ResultRow {
        self.iter()
            .map(|(column, value)| (column.to_string(), value.clone()))
            .collect()
    }
}

impl RowLookup for PhysicalRowView<'_> {
    fn column(&self, name: &str) -> Option<&Value> {
        self.schema
            .column_slot(name)
            .and_then(|slot| self.row.value(slot))
    }

    fn column_is_ambiguous(&self, name: &str) -> bool {
        self.schema.column_is_ambiguous(name)
    }

    fn qualified_column(&self, qualifier: &str, column: &str, key: &str) -> Option<&Value> {
        self.schema
            .qualified_slot(qualifier, column, key)
            .and_then(|slot| self.row.value(slot))
    }

    fn positional_column(&self, index: usize) -> Option<&Value> {
        self.value_at(index)
    }

    fn visit_columns(&self, visitor: &mut dyn FnMut(&str, &Value)) {
        for (column, value) in self.iter() {
            visitor(column, value);
        }
    }

    fn visit_lookup_columns(&self, visitor: &mut dyn FnMut(&str, &Value)) {
        self.visit_columns(visitor);
        for (alias, slot) in &self.schema.index.aliases {
            if *slot != NULL_SLOT {
                if let Some(value) = self.row.value(*slot) {
                    visitor(alias, value);
                }
            }
        }
    }
}

/// Owned schema/row pair for row-at-a-time consumers that must outlive a
/// decoded batch. Cloning this carrier shares the immutable schema index and
/// row fragments; it does not build a named row or clone contained values.
#[derive(Debug, Clone, PartialEq)]
pub struct OwnedPhysicalRow {
    pub schema: RowSchema,
    pub row: PhysicalRow,
}

impl OwnedPhysicalRow {
    pub fn new(schema: RowSchema, row: PhysicalRow) -> Self {
        Self { schema, row }
    }

    pub fn view(&self) -> PhysicalRowView<'_> {
        self.schema.view(&self.row)
    }

    pub fn get(&self, name: &str) -> Option<&Value> {
        self.schema
            .exact_slot(name)
            .and_then(|slot| self.row.value(slot))
    }

    pub fn into_result_row(self) -> ResultRow {
        self.schema.materialize_result_row(self.row)
    }
}

impl RowLookup for OwnedPhysicalRow {
    fn column(&self, name: &str) -> Option<&Value> {
        self.schema
            .column_slot(name)
            .and_then(|slot| self.row.value(slot))
    }

    fn column_is_ambiguous(&self, name: &str) -> bool {
        self.schema.column_is_ambiguous(name)
    }

    fn qualified_column(&self, qualifier: &str, column: &str, key: &str) -> Option<&Value> {
        self.schema
            .qualified_slot(qualifier, column, key)
            .and_then(|slot| self.row.value(slot))
    }

    fn positional_column(&self, index: usize) -> Option<&Value> {
        self.schema
            .slot(index)
            .and_then(|slot| self.row.value(slot))
    }

    fn visit_columns(&self, visitor: &mut dyn FnMut(&str, &Value)) {
        self.view().visit_columns(visitor);
    }

    fn visit_lookup_columns(&self, visitor: &mut dyn FnMut(&str, &Value)) {
        self.view().visit_lookup_columns(visitor);
    }
}

/// A schema and bounded vector of physical rows flowing between operators.
#[derive(Debug, Clone, PartialEq)]
pub struct Batch {
    pub schema: RowSchema,
    pub rows: Vec<PhysicalRow>,
}

impl Batch {
    /// Compatibility constructor for named rows entering the physical engine.
    /// The resulting batch is positional immediately; maps do not flow to the
    /// next operator.
    pub fn new(schema: RowSchema, rows: Vec<ResultRow>) -> Self {
        let schema = RowSchema::new(schema.columns().to_vec());
        let rows = rows
            .into_iter()
            .map(|row| PhysicalRow::from_result_row(&schema, row))
            .collect();
        Self { schema, rows }
    }

    pub fn from_physical_rows(schema: RowSchema, rows: Vec<PhysicalRow>) -> Self {
        debug_assert!(rows.iter().all(|row| {
            row.fragments.iter().map(RowFragment::len).sum::<usize>() == schema.physical_width()
        }));
        Self { schema, rows }
    }

    pub fn empty(schema: RowSchema) -> Self {
        Self {
            schema,
            rows: Vec::new(),
        }
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    pub fn into_result_rows(self) -> Vec<ResultRow> {
        let schema = self.schema;
        self.rows
            .into_iter()
            .map(|row| schema.materialize_result_row(row))
            .collect()
    }

    /// Split named rows into batches of at most [`DEFAULT_BATCH_SIZE`].
    pub fn chunked(schema: RowSchema, rows: Vec<ResultRow>) -> Vec<Batch> {
        if rows.is_empty() {
            return vec![Batch::empty(schema)];
        }
        let mut out = Vec::with_capacity(rows.len().div_ceil(DEFAULT_BATCH_SIZE));
        let mut buf = Vec::with_capacity(DEFAULT_BATCH_SIZE);
        for row in rows {
            buf.push(row);
            if buf.len() == DEFAULT_BATCH_SIZE {
                out.push(Batch::new(schema.clone(), std::mem::take(&mut buf)));
                buf.reserve(DEFAULT_BATCH_SIZE);
            }
        }
        if !buf.is_empty() {
            out.push(Batch::new(schema, buf));
        }
        out
    }
}

#[cfg(test)]
mod tests;
