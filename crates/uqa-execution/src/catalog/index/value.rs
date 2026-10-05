//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Predicate eligibility, NULL handling and evaluated-key retention for column and expression accelerators.
//!
//! An accelerator holds the stored value of its field for every row. A search key also orders its non-null values in a B-tree for predicates. A column that an index only carries beside its key holds the values alone: it answers the projection of an index-only read and no predicate, and a write maintains no value bucket for it.

use std::collections::BTreeMap;
use uqa_core::{DocId, Payload, PostingEntry, PostingList, Predicate, Value};
use uqa_storage::BTreeIndex;

mod comparison;

/// Per-column index: the stored value of every row, and for a search key
/// the non-null scalar keys in a B-tree plus the doc ids whose field is
/// missing or SQL NULL.
#[derive(Clone)]
pub struct ColumnValueIndex {
    /// `None` for a column that an index only carries.
    index: Option<BTreeIndex>,
    values: BTreeMap<DocId, Value>,
    /// Sorted doc ids with a missing or `Value::Null` field, kept for a search key.
    nulls: Vec<DocId>,
    /// Set when any indexed key is temporal; disables acceleration
    /// because string-vs-temporal comparisons need parsing.
    has_temporal: bool,
    has_fallible_comparison: bool,
    /// Set when any indexed key is a row value, whose null test inspects its fields; the null set cannot answer it, as `PostgreSQL` never indexes a row-type null test.
    has_row_values: bool,
}

fn value_is_row(value: &Value) -> bool {
    matches!(value, Value::Row(_) | Value::Record(_))
}

fn value_is_temporal(value: &Value) -> bool {
    matches!(value, Value::Temporal(_))
}

fn value_is_nan(value: &Value) -> bool {
    matches!(value, Value::Float(f) if f.is_nan())
}

fn predicate_targets_are_index_safe(predicate: &Predicate) -> bool {
    let safe = |v: &Value| !value_is_temporal(v) && !value_is_nan(v);
    match predicate {
        Predicate::Equals(v)
        | Predicate::NotEquals(v)
        | Predicate::GreaterThan(v)
        | Predicate::GreaterThanOrEqual(v)
        | Predicate::LessThan(v)
        | Predicate::LessThanOrEqual(v) => safe(v),
        Predicate::InSet(values) => values.iter().all(safe),
        Predicate::Between { low, high } => safe(low) && safe(high),
        Predicate::IsNull | Predicate::IsNotNull => true,
    }
}

impl ColumnValueIndex {
    /// Borrow the key evaluated for the stored row, without reevaluating an index expression against a newer routine definition.
    pub fn stored_value(&self, doc_id: DocId) -> Option<&Value> {
        self.values.get(&doc_id)
    }

    /// Whether a row is indexed, which every row of the table is while the accelerator is current.
    pub fn contains(&self, doc_id: DocId) -> bool {
        self.values.contains_key(&doc_id)
    }

    /// Whether this accelerator only carries its column's values and answers no predicate.
    pub fn is_carried(&self) -> bool {
        self.index.is_none()
    }

    /// Build a search key.
    pub fn build(field: &str, values: impl Iterator<Item = (DocId, Value)>) -> Self {
        let mut built = Self {
            index: Some(BTreeIndex::new(field)),
            values: BTreeMap::new(),
            nulls: Vec::new(),
            has_temporal: false,
            has_fallible_comparison: false,
            has_row_values: false,
        };
        for (doc_id, value) in values {
            built.index_value(doc_id, &value);
            built.values.insert(doc_id, value);
        }
        built.nulls.sort_unstable();
        built.nulls.dedup();
        built
    }

    /// Build the values of a column that an index only carries.
    pub fn build_carried(values: impl Iterator<Item = (DocId, Value)>) -> Self {
        Self {
            index: None,
            values: values.collect(),
            nulls: Vec::new(),
            has_temporal: false,
            has_fallible_comparison: false,
            has_row_values: false,
        }
    }

    /// The same stored values, carried or as a search key of `field`, when the indexes of the table give the column the other use.
    #[must_use]
    pub fn with_use(self, field: &str, carried: bool) -> Self {
        match (carried, self.is_carried()) {
            (false, true) => Self::build(field, self.values.into_iter()),
            (true, false) => Self::build_carried(self.values.into_iter()),
            _ => self,
        }
    }

    /// Order one stored value for predicates. `nulls` is appended to and sorted by the caller.
    fn index_value(&mut self, doc_id: DocId, value: &Value) {
        let Some(index) = self.index.as_mut() else {
            return;
        };
        match value {
            Value::Null => self.nulls.push(doc_id),
            value => {
                self.has_temporal |= value_is_temporal(value);
                self.has_row_values |= value_is_row(value);
                self.has_fallible_comparison |= uqa_sql::expr::value_comparison_can_fail(value);
                index.insert(doc_id, value.clone());
            }
        }
    }

    pub fn insert(&mut self, doc_id: DocId, value: &Value) {
        self.values.insert(doc_id, value.clone());
        let Some(index) = self.index.as_mut() else {
            return;
        };
        match value {
            Value::Null => {
                if let Err(pos) = self.nulls.binary_search(&doc_id) {
                    self.nulls.insert(pos, doc_id);
                }
            }
            value => {
                self.has_temporal |= value_is_temporal(value);
                self.has_row_values |= value_is_row(value);
                self.has_fallible_comparison |= uqa_sql::expr::value_comparison_can_fail(value);
                index.insert(doc_id, value.clone());
            }
        }
    }

    pub fn remove(&mut self, doc_id: DocId, value: &Value) {
        let stored = self.values.remove(&doc_id);
        let Some(index) = self.index.as_mut() else {
            return;
        };
        let value = stored.as_ref().unwrap_or(value);
        match value {
            Value::Null => {
                if let Ok(pos) = self.nulls.binary_search(&doc_id) {
                    self.nulls.remove(pos);
                }
            }
            value => index.remove(doc_id, value),
        }
    }

    pub fn clear(&mut self) {
        if let Some(index) = self.index.as_mut() {
            index.clear();
        }
        self.values.clear();
        self.nulls.clear();
        self.has_temporal = false;
        self.has_fallible_comparison = false;
        self.has_row_values = false;
    }

    /// Resolve `predicate` to a posting list, or `None` when this
    /// index cannot reproduce evaluated-scan semantics for it.
    pub fn scan(&self, predicate: &Predicate) -> Option<PostingList> {
        let index = self.index.as_ref()?;
        if !self.supports(predicate) {
            return None;
        }
        match predicate {
            Predicate::IsNull => Some(posting_list_from_sorted_ids(self.nulls.iter().copied())),
            Predicate::IsNotNull => Some(index.scan(&Predicate::IsNotNull)),
            // `NotEquals` needs "all non-null minus matches"; the
            // complement is rarely selective, so leave it to the scan.
            Predicate::NotEquals(_) => unreachable!("unsupported predicates return above"),
            predicate => Some(index.scan(predicate)),
        }
    }

    pub fn estimate_cardinality(&self, predicate: &Predicate) -> Option<usize> {
        let index = self.index.as_ref()?;
        if !self.supports(predicate) {
            return None;
        }
        Some(match predicate {
            Predicate::IsNull => self.nulls.len(),
            Predicate::IsNotNull => index.estimate_cardinality(predicate),
            Predicate::NotEquals(_) => unreachable!("unsupported predicates return above"),
            predicate => index.estimate_cardinality(predicate),
        })
    }

    /// The selected access path registers its logical predicate before reading even an empty posting list. Declined predicates do not register a read.
    pub fn scan_observing(
        &self,
        predicate: &Predicate,
        observe: impl FnOnce() -> Result<(), uqa_sql::SQLError>,
    ) -> Result<Option<PostingList>, uqa_sql::SQLError> {
        if self.is_carried() {
            return Ok(None);
        }
        if comparison::needs_sql_comparison(predicate, self.has_fallible_comparison) {
            observe()?;
            let mut ids = Vec::new();
            for (&id, value) in &self.values {
                if comparison::matches(value, predicate)? {
                    ids.push(id);
                }
            }
            return Ok(Some(posting_list_from_sorted_ids(ids.into_iter())));
        }
        if !self.supports(predicate) {
            return Ok(None);
        }
        observe()?;
        Ok(self.scan(predicate))
    }

    pub fn supports(&self, predicate: &Predicate) -> bool {
        !self.is_carried()
            && predicate_targets_are_index_safe(predicate)
            && !comparison::needs_sql_comparison(predicate, self.has_fallible_comparison)
            && !matches!(predicate, Predicate::NotEquals(_))
            && if matches!(predicate, Predicate::IsNull | Predicate::IsNotNull) {
                !self.has_row_values
            } else {
                !self.has_temporal
            }
    }
}

fn posting_list_from_sorted_ids(ids: impl Iterator<Item = DocId>) -> PostingList {
    let entries: Vec<PostingEntry> = ids
        .map(|doc_id| PostingEntry::new(doc_id, Payload::default()))
        .collect();
    PostingList::from_sorted_unchecked(entries)
}

#[cfg(test)]
mod tests;
