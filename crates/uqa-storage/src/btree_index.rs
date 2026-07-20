//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! In-memory B-tree index for predicate scans.
//!
//! Supports the comparison-style [`Predicate`] variants (`Equals`,
//! `Between`, `GreaterThan`, ...) by walking the underlying
//! [`BTreeMap`] in key order. The result is a [`PostingList`] with
//! `score = 0.0`, leaving downstream scoring to the operator pipeline.

use std::collections::BTreeMap;
use std::ops::Bound;

use uqa_core::{DocId, Payload, PostingEntry, PostingList, Predicate, Value};

fn sort_and_dedup_doc_ids(doc_ids: &mut Vec<DocId>) {
    if doc_ids.len() < 2 {
        return;
    }

    let mut min_doc_id = DocId::MAX;
    let mut max_doc_id = DocId::MIN;
    for doc_id in doc_ids.iter().copied() {
        min_doc_id = min_doc_id.min(doc_id);
        max_doc_id = max_doc_id.max(doc_id);
    }
    let dense_span = max_doc_id
        .checked_sub(min_doc_id)
        .and_then(|span| span.checked_add(1))
        .and_then(|span| usize::try_from(span).ok())
        .filter(|span| *span <= doc_ids.len().saturating_mul(8));

    let Some(span) = dense_span else {
        doc_ids.sort_unstable();
        doc_ids.dedup();
        return;
    };

    // At density >= 1/8, this bitmap is no larger than the input id
    // vector and turns comparison sorting into two linear, cache-local
    // passes. Sparse or very large id spaces stay on sort_unstable above.
    let mut words = vec![0u64; span.div_ceil(u64::BITS as usize)];
    for doc_id in doc_ids.iter().copied() {
        let offset = usize::try_from(doc_id - min_doc_id).expect("dense doc-id offset fits usize");
        words[offset / u64::BITS as usize] |= 1u64 << (offset % u64::BITS as usize);
    }

    doc_ids.clear();
    for (word_index, mut word) in words.into_iter().enumerate() {
        while word != 0 {
            let bit = word.trailing_zeros();
            doc_ids.push(min_doc_id + word_index as u64 * u64::from(u64::BITS) + u64::from(bit));
            word &= word - 1;
        }
    }
}

#[derive(Debug, Default, Clone)]
pub struct BTreeIndex {
    field: String,
    /// `Value -> sorted, deduped Vec<DocId>`. Equal values share the
    /// same key.
    entries: BTreeMap<Value, Vec<DocId>>,
}

impl BTreeIndex {
    pub fn new(field: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            entries: BTreeMap::new(),
        }
    }

    pub fn field(&self) -> &str {
        &self.field
    }

    pub fn insert(&mut self, doc_id: DocId, value: Value) {
        let bucket = self.entries.entry(value).or_default();
        if let Err(pos) = bucket.binary_search(&doc_id) {
            bucket.insert(pos, doc_id);
        }
    }

    pub fn remove(&mut self, doc_id: DocId, value: &Value) {
        let mut prune = false;
        if let Some(bucket) = self.entries.get_mut(value) {
            if let Ok(pos) = bucket.binary_search(&doc_id) {
                bucket.remove(pos);
            }
            prune = bucket.is_empty();
        }
        if prune {
            self.entries.remove(value);
        }
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// Run `predicate` against the index, returning the matching `doc_ids`
    /// as a sorted, deduplicated [`PostingList`].
    pub fn scan(&self, predicate: &Predicate) -> PostingList {
        let range_iter: Box<dyn Iterator<Item = (&Value, &Vec<DocId>)> + '_> = match predicate {
            Predicate::Equals(t) => match self.entries.get(t) {
                Some(bucket) => Box::new(std::iter::once((t, bucket))),
                None => Box::new(std::iter::empty()),
            },
            Predicate::GreaterThan(t) => Box::new(
                self.entries
                    .range((Bound::Excluded(t.clone()), Bound::Unbounded)),
            ),
            Predicate::GreaterThanOrEqual(t) => Box::new(
                self.entries
                    .range((Bound::Included(t.clone()), Bound::Unbounded)),
            ),
            Predicate::LessThan(t) => Box::new(
                self.entries
                    .range((Bound::Unbounded, Bound::Excluded(t.clone()))),
            ),
            Predicate::LessThanOrEqual(t) => Box::new(
                self.entries
                    .range((Bound::Unbounded, Bound::Included(t.clone()))),
            ),
            Predicate::Between { low, high } => Box::new(
                self.entries
                    .range((Bound::Included(low.clone()), Bound::Included(high.clone()))),
            ),
            Predicate::InSet(values) => Box::new(
                values
                    .iter()
                    .filter_map(move |v| self.entries.get_key_value(v)),
            ),
            Predicate::NotEquals(_) | Predicate::IsNull | Predicate::IsNotNull => {
                // Full scan with predicate fallback for complement-style
                // predicates. The caller is expected to combine the
                // result with a universal-set complement when more
                // appropriate.
                Box::new(self.entries.iter())
            }
        };

        let mut all_ids: Vec<DocId> = Vec::new();
        for (value, bucket) in range_iter {
            // For predicates that need element-wise checks, evaluate
            // here so the result honours the trait contract.
            let keep = match predicate {
                Predicate::NotEquals(_) | Predicate::IsNotNull => predicate.evaluate(Some(value)),
                Predicate::IsNull => false, // a value is present, so IsNull never matches
                _ => true,
            };
            if keep {
                all_ids.extend_from_slice(bucket);
            }
        }
        sort_and_dedup_doc_ids(&mut all_ids);
        let entries = all_ids
            .into_iter()
            .map(|doc_id| PostingEntry::new(doc_id, Payload::default()))
            .collect();
        PostingList::from_sorted_unchecked(entries)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn idx_with_ints() -> BTreeIndex {
        let mut idx = BTreeIndex::new("year");
        idx.insert(1, Value::Int(2020));
        idx.insert(2, Value::Int(2022));
        idx.insert(3, Value::Int(2025));
        idx.insert(4, Value::Int(2025));
        idx.insert(5, Value::Int(2030));
        idx
    }

    fn ids(pl: &PostingList) -> Vec<DocId> {
        pl.iter().map(|e| e.doc_id).collect()
    }

    #[test]
    fn equals_returns_exact_matches() {
        let idx = idx_with_ints();
        let pl = idx.scan(&Predicate::Equals(Value::Int(2025)));
        assert_eq!(ids(&pl), vec![3, 4]);
    }

    #[test]
    fn greater_than_excludes_endpoint() {
        let idx = idx_with_ints();
        let pl = idx.scan(&Predicate::GreaterThan(Value::Int(2022)));
        assert_eq!(ids(&pl), vec![3, 4, 5]);
    }

    #[test]
    fn between_inclusive_bounds() {
        let idx = idx_with_ints();
        let pl = idx.scan(&Predicate::Between {
            low: Value::Int(2022),
            high: Value::Int(2025),
        });
        assert_eq!(ids(&pl), vec![2, 3, 4]);
    }

    #[test]
    fn remove_evicts_doc_id_and_empty_bucket() {
        let mut idx = idx_with_ints();
        idx.remove(3, &Value::Int(2025));
        let pl = idx.scan(&Predicate::Equals(Value::Int(2025)));
        assert_eq!(ids(&pl), vec![4]);
        idx.remove(4, &Value::Int(2025));
        let pl = idx.scan(&Predicate::Equals(Value::Int(2025)));
        assert!(pl.is_empty());
    }

    #[test]
    fn in_set_returns_union_of_buckets() {
        let idx = idx_with_ints();
        let mut s = std::collections::BTreeSet::new();
        s.insert(Value::Int(2020));
        s.insert(Value::Int(2030));
        let pl = idx.scan(&Predicate::InSet(s));
        assert_eq!(ids(&pl), vec![1, 5]);
    }

    #[test]
    fn dense_scan_deduplicates_docs_present_in_multiple_value_buckets() {
        let mut idx = idx_with_ints();
        idx.insert(3, Value::Int(2030));

        let pl = idx.scan(&Predicate::Between {
            low: Value::Int(2020),
            high: Value::Int(2030),
        });
        assert_eq!(ids(&pl), vec![1, 2, 3, 4, 5]);
    }

    #[test]
    fn sparse_extreme_doc_ids_fall_back_to_comparison_sort() {
        let mut doc_ids = vec![DocId::MAX, 0, 42, 0];

        sort_and_dedup_doc_ids(&mut doc_ids);

        assert_eq!(doc_ids, vec![0, 42, DocId::MAX]);
    }
}
