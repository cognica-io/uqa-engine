//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Lazily built, incrementally maintained per-column value indexes.
//!
//! Scalar WHERE predicates historically evaluated by scanning every
//! document. A [`ColumnValueIndex`] wraps the storage-layer
//! [`BTreeIndex`] so equality / range / IN / IS NULL predicates on
//! indexed columns resolve to a [`PostingList`] in `O(log n + k)` and
//! then compose with the posting-list Boolean algebra like any other
//! signal. Indexes are built on first use from one bulk field scan and
//! maintained incrementally by the insert / update / delete paths.
//!
//! Only columns the catalog marks as indexable get an index: PRIMARY
//! KEY and UNIQUE columns, and columns covered by a `CREATE INDEX ...
//! USING btree` entry (first column of a composite index). This keeps
//! write amplification bounded and mirrors `PostgreSQL`, where those
//! are exactly the columns with implicit or explicit b-tree indexes.
//!
//! ## Semantics guard
//!
//! [`Predicate::evaluate`] compares temporal values against strings by
//! parsing, and `f64` NaN never equals itself; a raw `BTreeMap` lookup
//! cannot reproduce either. `scan` therefore refuses (returns `None`)
//! whenever the index contains temporal keys or the predicate target
//! is temporal or NaN, and callers fall back to the evaluated scan, so
//! an index lookup can never change query results.

use std::collections::BTreeMap;

use uqa_core::{DocId, Payload, PostingEntry, PostingList, Predicate, Value};
use uqa_storage::BTreeIndex;

use crate::TableState;

/// Per-column index: non-null scalar keys in a B-tree plus the doc ids
/// whose field is missing or SQL NULL.
pub(crate) struct ColumnValueIndex {
    index: BTreeIndex,
    /// Sorted doc ids with a missing or `Value::Null` field.
    nulls: Vec<DocId>,
    /// Set when any indexed key is temporal; disables acceleration
    /// because string-vs-temporal comparisons need parsing.
    has_temporal: bool,
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
    pub(crate) fn build(field: &str, values: impl Iterator<Item = (DocId, Value)>) -> Self {
        let mut index = BTreeIndex::new(field);
        let mut nulls = Vec::new();
        let mut has_temporal = false;
        for (doc_id, value) in values {
            match value {
                Value::Null => nulls.push(doc_id),
                value => {
                    has_temporal |= value_is_temporal(&value);
                    index.insert(doc_id, value);
                }
            }
        }
        nulls.sort_unstable();
        nulls.dedup();
        Self {
            index,
            nulls,
            has_temporal,
        }
    }

    pub(crate) fn insert(&mut self, doc_id: DocId, value: &Value) {
        match value {
            Value::Null => {
                if let Err(pos) = self.nulls.binary_search(&doc_id) {
                    self.nulls.insert(pos, doc_id);
                }
            }
            value => {
                self.has_temporal |= value_is_temporal(value);
                self.index.insert(doc_id, value.clone());
            }
        }
    }

    pub(crate) fn remove(&mut self, doc_id: DocId, value: &Value) {
        match value {
            Value::Null => {
                if let Ok(pos) = self.nulls.binary_search(&doc_id) {
                    self.nulls.remove(pos);
                }
            }
            value => self.index.remove(doc_id, value),
        }
    }

    /// Resolve `predicate` to a posting list, or `None` when this
    /// index cannot reproduce evaluated-scan semantics for it.
    pub(crate) fn scan(&self, predicate: &Predicate) -> Option<PostingList> {
        if !predicate_targets_are_index_safe(predicate) {
            return None;
        }
        match predicate {
            Predicate::IsNull => Some(posting_list_from_sorted_ids(self.nulls.iter().copied())),
            Predicate::IsNotNull => Some(self.index.scan(&Predicate::IsNotNull)),
            // `NotEquals` needs "all non-null minus matches"; the
            // complement is rarely selective, so leave it to the scan.
            Predicate::NotEquals(_) => None,
            predicate => {
                if self.has_temporal {
                    return None;
                }
                Some(self.index.scan(predicate))
            }
        }
    }
}

fn posting_list_from_sorted_ids(ids: impl Iterator<Item = DocId>) -> PostingList {
    let entries: Vec<PostingEntry> = ids
        .map(|doc_id| PostingEntry::new(doc_id, Payload::default()))
        .collect();
    PostingList::from_sorted_unchecked(entries)
}

impl crate::Engine {
    /// Columns of `table` that qualify for a value index: PRIMARY KEY
    /// and UNIQUE columns plus the leading column of every btree
    /// `CREATE INDEX` on the table.
    pub(crate) fn value_indexable_fields(&self, table: &str) -> Vec<String> {
        let mut fields = Vec::new();
        if let Some(t) = self.table(table) {
            for column in t.columns.read().iter() {
                if (column.primary_key || column.unique) && !fields.contains(&column.name) {
                    fields.push(column.name.clone());
                }
            }
        }
        let resolved = self.resolve_table_name(table);
        for row in self.catalog_indexes.read().values() {
            if !row.index_type.eq_ignore_ascii_case("btree") {
                continue;
            }
            let matches_table = row.table_name == table
                || resolved
                    .as_deref()
                    .is_some_and(|name| row.table_name == name);
            if !matches_table {
                continue;
            }
            let columns: Vec<String> = serde_json::from_str(&row.columns_json).unwrap_or_default();
            if let Some(first) = columns.first() {
                if !fields.contains(first) {
                    fields.push(first.clone());
                }
            }
        }
        fields
    }

    /// Resolve a scalar predicate on `field` through a value index.
    /// Returns `None` when the column has no index policy, the index
    /// cannot reproduce scan semantics, or the table is unknown.
    pub(crate) fn value_index_scan(
        &self,
        table: &str,
        field: &str,
        predicate: &Predicate,
    ) -> Option<PostingList> {
        let t = self.table(table)?;
        {
            let indexes = t.value_indexes.read();
            if let Some(index) = indexes.get(field) {
                return index.scan(predicate);
            }
        }
        if !self
            .value_indexable_fields(table)
            .iter()
            .any(|name| name == field)
        {
            return None;
        }
        // Build outside the write lock from one bulk field scan, then
        // publish. A concurrent builder doing the same work is
        // harmless: last write wins with identical content.
        let doc_ids = self.table_doc_ids(table);
        let values = self.get_document_fields(table, &doc_ids, field);
        let built = ColumnValueIndex::build(
            field,
            doc_ids
                .iter()
                .map(|doc_id| (*doc_id, values.get(doc_id).cloned().unwrap_or(Value::Null))),
        );
        let mut indexes = t.value_indexes.write();
        let index = indexes.entry(field.to_string()).or_insert(built);
        index.scan(predicate)
    }

    /// Incremental maintenance for built indexes. `old` carries the
    /// previous field values when the document already existed.
    pub(crate) fn value_indexes_apply_write(
        t: &TableState,
        doc_id: DocId,
        old: Option<&BTreeMap<String, Value>>,
        new: Option<&BTreeMap<String, Value>>,
    ) {
        let mut indexes = t.value_indexes.write();
        if indexes.is_empty() {
            return;
        }
        for (field, index) in indexes.iter_mut() {
            if let Some(old_values) = old {
                index.remove(doc_id, old_values.get(field).unwrap_or(&Value::Null));
            }
            if let Some(new_values) = new {
                index.insert(doc_id, new_values.get(field).unwrap_or(&Value::Null));
            }
        }
    }

    /// Fetch the previous values of every built-index field for
    /// `doc_id`, so a write can unindex them. Returns `None` when no
    /// indexes are built (the common case, costing one read-lock).
    pub(crate) fn value_indexes_old_values(
        t: &TableState,
        doc_id: DocId,
    ) -> Option<BTreeMap<String, Value>> {
        let fields: Vec<String> = {
            let indexes = t.value_indexes.read();
            if indexes.is_empty() {
                return None;
            }
            indexes.keys().cloned().collect()
        };
        // One point read of the whole document beats per-field JSON
        // extraction on the write path.
        let document = t.document_store.read().get(doc_id).unwrap_or_default();
        Some(
            fields
                .into_iter()
                .map(|field| {
                    let value = document.get(&field).cloned().unwrap_or(Value::Null);
                    (field, value)
                })
                .collect(),
        )
    }

    /// Drop every built index for the table (TRUNCATE, bulk reloads,
    /// store replacement, schema changes).
    pub(crate) fn value_indexes_clear(t: &TableState) {
        t.value_indexes.write().clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(list: &PostingList) -> Vec<DocId> {
        list.entries().iter().map(|e| e.doc_id).collect()
    }

    #[test]
    fn build_scan_equals_and_ranges() {
        let index = ColumnValueIndex::build(
            "qty",
            vec![
                (1, Value::Int(10)),
                (2, Value::Int(20)),
                (3, Value::Int(20)),
                (4, Value::Null),
                (5, Value::Int(30)),
            ]
            .into_iter(),
        );
        assert_eq!(
            ids(&index.scan(&Predicate::Equals(Value::Int(20))).unwrap()),
            vec![2, 3]
        );
        assert_eq!(
            ids(&index.scan(&Predicate::GreaterThan(Value::Int(10))).unwrap()),
            vec![2, 3, 5]
        );
        assert_eq!(
            ids(&index
                .scan(&Predicate::Between {
                    low: Value::Int(10),
                    high: Value::Int(20),
                })
                .unwrap()),
            vec![1, 2, 3]
        );
        assert_eq!(ids(&index.scan(&Predicate::IsNull).unwrap()), vec![4]);
        assert_eq!(
            ids(&index.scan(&Predicate::IsNotNull).unwrap()),
            vec![1, 2, 3, 5]
        );
        assert!(index.scan(&Predicate::NotEquals(Value::Int(10))).is_none());
    }

    #[test]
    fn incremental_insert_remove_tracks_nulls() {
        let mut index = ColumnValueIndex::build("qty", std::iter::empty());
        index.insert(7, &Value::Int(1));
        index.insert(8, &Value::Null);
        assert_eq!(
            ids(&index.scan(&Predicate::Equals(Value::Int(1))).unwrap()),
            vec![7]
        );
        assert_eq!(ids(&index.scan(&Predicate::IsNull).unwrap()), vec![8]);
        index.remove(7, &Value::Int(1));
        index.remove(8, &Value::Null);
        assert!(ids(&index.scan(&Predicate::Equals(Value::Int(1))).unwrap()).is_empty());
        assert!(ids(&index.scan(&Predicate::IsNull).unwrap()).is_empty());
    }

    #[test]
    fn temporal_and_nan_guards_refuse_acceleration() {
        let temporal = uqa_core::TemporalValue::parse_date("2024-01-01").unwrap();
        let index = ColumnValueIndex::build(
            "ts",
            vec![(1, Value::Temporal(temporal.clone()))].into_iter(),
        );
        assert!(index
            .scan(&Predicate::Equals(Value::Str("2024-01-01".into())))
            .is_none());

        let numeric = ColumnValueIndex::build("f", vec![(1, Value::Float(1.0))].into_iter());
        assert!(numeric
            .scan(&Predicate::Equals(Value::Float(f64::NAN)))
            .is_none());
        assert!(numeric
            .scan(&Predicate::Equals(Value::Temporal(temporal)))
            .is_none());
    }
}
