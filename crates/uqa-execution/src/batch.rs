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

impl RowSchema {
    pub fn new(columns: Vec<String>) -> Self {
        let width = columns.len();
        Self::from_parts(columns, (0..width).collect(), width)
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
        debug_assert_eq!(columns.len(), slots.len());
        let mut exact = HashMap::with_capacity(columns.len());
        let mut qualified: HashMap<Box<str>, HashMap<Box<str>, usize>> = HashMap::new();
        let mut alias_qualified: HashMap<Box<str>, HashMap<Box<str>, usize>> = HashMap::new();
        let mut suffix_candidates: HashMap<Box<str>, (String, usize)> = HashMap::new();
        let mut qualified_owners = HashSet::new();

        for (logical, name) in columns.iter().enumerate() {
            // Later writes to the same named field replace the value in a
            // ResultRow. Schema transforms preserve that contract.
            exact.insert(Box::<str>::from(name.as_str()), logical);
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

    pub fn position(&self, name: &str) -> Option<usize> {
        self.index.exact.get(name).copied()
    }

    pub fn physical_width(&self) -> usize {
        self.index.physical_width
    }

    fn slot(&self, logical: usize) -> Option<usize> {
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
        self.exact_slot(name).or_else(|| {
            self.index
                .suffix
                .get(name)
                .and_then(|logical| self.slot(*logical))
        })
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
        Self::from_parts(output_names, slots, input.physical_width())
    }

    /// Add hidden lookup names for existing slots. These aliases participate
    /// in column resolution but not schema iteration or final materialization.
    pub fn with_lookup_aliases(input: &Self, aliases: &[(String, String)]) -> Self {
        let mut lookup_aliases = input.index.aliases.clone();
        for (alias, source) in aliases {
            lookup_aliases.insert(
                Box::<str>::from(alias.as_str()),
                input.exact_slot(source).unwrap_or(NULL_SLOT),
            );
        }
        Self::from_parts_with_aliases(
            input.columns().to_vec(),
            input.index.slots.to_vec(),
            input.physical_width(),
            lookup_aliases,
        )
    }

    /// Append freshly-computed values to an existing physical row. Reusing an
    /// existing output name replaces its logical slot just like map insertion.
    pub fn append(input: &Self, names: &[String]) -> Self {
        let mut columns = input.columns().to_vec();
        let mut slots = input.index.slots.to_vec();
        let base = input.physical_width();
        for (offset, name) in names.iter().enumerate() {
            let slot = base + offset;
            if let Some(position) = columns.iter().position(|column| column == name) {
                slots[position] = slot;
            } else {
                columns.push(name.clone());
                slots.push(slot);
            }
        }
        Self::from_parts_with_aliases(
            columns,
            slots,
            base + names.len(),
            input.index.aliases.clone(),
        )
    }

    /// Compose two child layouts. The right side replaces duplicate field
    /// names, matching `ResultRow::insert`, while all value fragments remain
    /// shared.
    pub fn join(
        left: &Self,
        right: &Self,
        extra_columns: impl IntoIterator<Item = String>,
    ) -> Self {
        let mut columns = left.columns().to_vec();
        let mut slots = left.index.slots.to_vec();
        let right_base = left.physical_width();
        let mut aliases = left.index.aliases.clone();
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
        for (right_logical, column) in right.columns().iter().enumerate() {
            let slot = right
                .slot(right_logical)
                .map_or(NULL_SLOT, |slot| right_base + slot);
            if let Some(position) = columns.iter().position(|existing| existing == column) {
                slots[position] = slot;
            } else {
                columns.push(column.clone());
                slots.push(slot);
            }
        }
        for column in extra_columns {
            if !columns.contains(&column) {
                columns.push(column);
                slots.push(NULL_SLOT);
            }
        }
        Self::from_parts_with_aliases(
            columns,
            slots,
            left.physical_width() + right.physical_width(),
            aliases,
        )
    }

    pub fn view<'a>(&'a self, row: &'a PhysicalRow) -> PhysicalRowView<'a> {
        PhysicalRowView { schema: self, row }
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

    fn value(&self, mut slot: usize) -> Option<&Value> {
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
    fn project_slots(&self, slots: &[usize]) -> Self {
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

    /// Canonicalize a materialization boundary to one positional slot per
    /// logical column. Composite input fragments remain shared in memory;
    /// only the slot projections are rebuilt. This gives repeatable spill/CTE
    /// scans one stable physical schema regardless of whether the buffer later
    /// crosses to disk.
    pub(crate) fn into_canonical(self, columns: &[String]) -> ExecResult<Self> {
        if self.schema.columns() != columns {
            return Err(ExecError::Other(format!(
                "materialized batch schema mismatch: expected {:?}, got {:?}",
                columns,
                self.schema.columns()
            )));
        }
        let schema = RowSchema::new(columns.to_vec());
        let slots = self.schema.index.slots.to_vec();
        let identity = self.schema.physical_width() == slots.len()
            && slots.iter().enumerate().all(|(index, slot)| index == *slot);
        let rows = if identity {
            self.rows
        } else {
            self.rows
                .iter()
                .map(|row| row.project_slots(&slots))
                .collect()
        };
        Ok(Self::from_physical_rows(schema, rows))
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
            .iter()
            .map(|row| schema.view(row).to_result_row())
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
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn qualified_schema_lookup_reads_positional_values() {
        let schema = RowSchema::new(vec!["orders.id".into(), "customer.id".into()]);
        let row = PhysicalRow::from_values(vec![Value::Int(1), Value::Int(2)]);
        let view = schema.view(&row);
        assert_eq!(
            view.qualified_column("orders", "id", "orders.id"),
            Some(&Value::Int(1))
        );
        assert_eq!(
            view.qualified_column("customer", "id", "customer.id"),
            Some(&Value::Int(2))
        );
    }

    #[test]
    fn join_composes_fragments_without_cloning_values() {
        let left_schema = RowSchema::new(vec!["l.value".into()]);
        let right_schema = RowSchema::new(vec!["r.value".into()]);
        let output_schema = RowSchema::join(&left_schema, &right_schema, Vec::new());
        let left = PhysicalRow::from_values(vec![Value::Str("left".repeat(128))]);
        let right = PhysicalRow::from_values(vec![Value::Str("right".repeat(128))]);
        let left_fragment = Arc::clone(&left.fragments[0].values);
        let right_fragment = Arc::clone(&right.fragments[0].values);

        let joined = PhysicalRow::concat(&left, &right);

        assert_eq!(joined.fragment_count(), 2);
        assert!(Arc::ptr_eq(&joined.fragments[0].values, &left_fragment));
        assert!(Arc::ptr_eq(&joined.fragments[1].values, &right_fragment));
        let view = output_schema.view(&joined);
        assert_eq!(view.get("l.value"), left_fragment.first());
        assert_eq!(view.get("r.value"), right_fragment.first());
    }

    #[test]
    fn selection_renames_by_remapping_slots() {
        let input = RowSchema::new(vec!["source".into(), "value".into()]);
        let output = RowSchema::select(&input, &[("renamed".into(), "value".into())]);
        let row = PhysicalRow::from_values(vec![Value::Int(1), Value::Int(2)]);
        assert_eq!(output.view(&row).get("renamed"), Some(&Value::Int(2)));
        assert_eq!(output.physical_width(), 2);
    }

    #[test]
    fn materialization_canonicalizes_slots_without_cloning_values() {
        let input = RowSchema::new(vec!["source".into(), "value".into()]);
        let selected = RowSchema::select(&input, &[("renamed".into(), "value".into())]);
        let row =
            PhysicalRow::from_values(vec![Value::Int(1), Value::Str("shared payload".repeat(32))]);
        let values = Arc::clone(&row.fragments[0].values);

        let batch = Batch::from_physical_rows(selected, vec![row])
            .into_canonical(&["renamed".into()])
            .unwrap();

        assert_eq!(batch.schema.physical_width(), 1);
        assert_eq!(
            batch.schema.view(&batch.rows[0]).get("renamed"),
            values.get(1)
        );
        assert!(Arc::ptr_eq(&batch.rows[0].fragments[0].values, &values));
    }

    #[test]
    fn shared_storage_projection_remaps_without_cloning_values() {
        let stored = Arc::new(vec![Value::Str("alpha".repeat(64)), Value::Int(7)]);
        let row =
            PhysicalRow::from_shared_values(Arc::clone(&stored), Arc::from([1, NULL_SLOT, 0]));
        let schema = RowSchema::new(vec!["number".into(), "missing".into(), "text".into()]);
        let view = schema.view(&row);

        assert!(Arc::ptr_eq(&row.fragments[0].values, &stored));
        assert_eq!(view.get("number"), Some(&Value::Int(7)));
        assert_eq!(view.get("missing"), Some(&Value::Null));
        assert_eq!(view.get("text"), stored.first());
    }

    #[test]
    fn lookup_aliases_share_a_slot_without_becoming_output_columns() {
        let input = RowSchema::new(vec!["id".into()]);
        let schema = RowSchema::with_lookup_aliases(&input, &[("orders.id".into(), "id".into())]);
        let row = PhysicalRow::from_values(vec![Value::Int(7)]);
        let view = schema.view(&row);

        assert_eq!(schema.columns(), ["id"]);
        assert_eq!(schema.physical_width(), 1);
        assert_eq!(view.get("orders.id"), Some(&Value::Int(7)));
        assert_eq!(
            view.qualified_column("orders", "id", "orders.id"),
            Some(&Value::Int(7))
        );
        assert_eq!(
            view.to_result_row(),
            BTreeMap::from([("id".into(), Value::Int(7))])
        );
    }

    #[test]
    fn result_materialization_happens_only_at_explicit_boundary() {
        let schema = RowSchema::new(vec!["a".into(), "b".into()]);
        let batch = Batch::from_physical_rows(
            schema,
            vec![PhysicalRow::from_values(vec![Value::Int(1), Value::Int(2)])],
        );
        assert_eq!(
            batch.into_result_rows(),
            vec![BTreeMap::from([
                ("a".into(), Value::Int(1)),
                ("b".into(), Value::Int(2)),
            ])]
        );
    }
}
