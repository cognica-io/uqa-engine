//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Predicate eligibility, NULL handling and evaluated-key retention for column and expression accelerators.

use std::collections::BTreeMap;
use uqa_core::{DocId, Payload, PostingEntry, PostingList, Predicate, Value};
use uqa_storage::BTreeIndex;

mod comparison;

/// Per-column index: non-null scalar keys in a B-tree plus the doc ids
/// whose field is missing or SQL NULL.
#[derive(Clone)]
pub struct ColumnValueIndex {
    index: BTreeIndex,
    values: BTreeMap<DocId, Value>,
    /// Sorted doc ids with a missing or `Value::Null` field.
    nulls: Vec<DocId>,
    /// Set when any indexed key is temporal; disables acceleration
    /// because string-vs-temporal comparisons need parsing.
    has_temporal: bool,
    has_fallible_comparison: bool,
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

    pub fn build(field: &str, values: impl Iterator<Item = (DocId, Value)>) -> Self {
        let mut index = BTreeIndex::new(field);
        let mut stored = BTreeMap::new();
        let mut nulls = Vec::new();
        let mut has_temporal = false;
        let mut has_fallible_comparison = false;
        for (doc_id, value) in values {
            stored.insert(doc_id, value.clone());
            match value {
                Value::Null => nulls.push(doc_id),
                value => {
                    has_temporal |= value_is_temporal(&value);
                    has_fallible_comparison |= uqa_sql::expr::value_comparison_can_fail(&value);
                    index.insert(doc_id, value);
                }
            }
        }
        nulls.sort_unstable();
        nulls.dedup();
        Self {
            index,
            values: stored,
            nulls,
            has_temporal,
            has_fallible_comparison,
        }
    }

    pub fn insert(&mut self, doc_id: DocId, value: &Value) {
        self.values.insert(doc_id, value.clone());
        match value {
            Value::Null => {
                if let Err(pos) = self.nulls.binary_search(&doc_id) {
                    self.nulls.insert(pos, doc_id);
                }
            }
            value => {
                self.has_temporal |= value_is_temporal(value);
                self.has_fallible_comparison |= uqa_sql::expr::value_comparison_can_fail(value);
                self.index.insert(doc_id, value.clone());
            }
        }
    }

    pub fn remove(&mut self, doc_id: DocId, value: &Value) {
        let stored = self.values.remove(&doc_id);
        let value = stored.as_ref().unwrap_or(value);
        match value {
            Value::Null => {
                if let Ok(pos) = self.nulls.binary_search(&doc_id) {
                    self.nulls.remove(pos);
                }
            }
            value => self.index.remove(doc_id, value),
        }
    }

    pub fn clear(&mut self) {
        self.index.clear();
        self.values.clear();
        self.nulls.clear();
        self.has_temporal = false;
        self.has_fallible_comparison = false;
    }

    /// Resolve `predicate` to a posting list, or `None` when this
    /// index cannot reproduce evaluated-scan semantics for it.
    pub fn scan(&self, predicate: &Predicate) -> Option<PostingList> {
        if !self.supports(predicate) {
            return None;
        }
        match predicate {
            Predicate::IsNull => Some(posting_list_from_sorted_ids(self.nulls.iter().copied())),
            Predicate::IsNotNull => Some(self.index.scan(&Predicate::IsNotNull)),
            // `NotEquals` needs "all non-null minus matches"; the
            // complement is rarely selective, so leave it to the scan.
            Predicate::NotEquals(_) => unreachable!("unsupported predicates return above"),
            predicate => Some(self.index.scan(predicate)),
        }
    }

    pub fn estimate_cardinality(&self, predicate: &Predicate) -> Option<usize> {
        if !self.supports(predicate) {
            return None;
        }
        Some(match predicate {
            Predicate::IsNull => self.nulls.len(),
            Predicate::IsNotNull => self.index.estimate_cardinality(predicate),
            Predicate::NotEquals(_) => unreachable!("unsupported predicates return above"),
            predicate => self.index.estimate_cardinality(predicate),
        })
    }

    /// The selected access path registers its logical predicate before reading even an empty posting list. Declined predicates do not register a read.
    pub fn scan_observing(
        &self,
        predicate: &Predicate,
        observe: impl FnOnce() -> Result<(), uqa_sql::SQLError>,
    ) -> Result<Option<PostingList>, uqa_sql::SQLError> {
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
        predicate_targets_are_index_safe(predicate)
            && !comparison::needs_sql_comparison(predicate, self.has_fallible_comparison)
            && !matches!(predicate, Predicate::NotEquals(_))
            && (matches!(predicate, Predicate::IsNull | Predicate::IsNotNull) || !self.has_temporal)
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
