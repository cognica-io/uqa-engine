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

use std::collections::{BTreeMap, BTreeSet};

use uqa_core::{DocId, Payload, PostingEntry, PostingList, Predicate, Value};
use uqa_storage::BTreeIndex;

use crate::{SQLError, StorageBackendError, StorageBackendResult, TableState};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MissingValueIndexMode {
    /// Build an in-memory accelerator from the pinned document snapshot, but
    /// leave durable storage untouched. Query execution and rollback recovery
    /// use this mode so a read transaction can never be upgraded by an index
    /// cache miss.
    MemoryOnly,
    /// Materialize the complete durable posting set when it is absent. Only
    /// DDL and the explicit open-time repair boundary may use this mode.
    Persist,
}

#[derive(Debug, Default, PartialEq, Eq)]
struct PersistentValueIndexRepairPlan {
    /// Legacy unqualified table keys whose complete durable posting sets must
    /// be removed before their canonical counterparts are rebuilt.
    aliases: BTreeSet<String>,
    /// Canonical tables whose durable marker fields differ from catalog policy
    /// or which had a legacy alias.
    tables: BTreeSet<String>,
}

impl PersistentValueIndexRepairPlan {
    fn is_empty(&self) -> bool {
        self.aliases.is_empty() && self.tables.is_empty()
    }
}

fn unqualified_relation_key(qualified: &str) -> Option<&str> {
    let mut quoted = false;
    let mut chars = qualified.char_indices().peekable();
    while let Some((index, ch)) = chars.next() {
        if ch == '"' {
            if quoted && chars.peek().is_some_and(|(_, next)| *next == '"') {
                chars.next();
            } else {
                quoted = !quoted;
            }
        } else if ch == '.' && !quoted {
            return Some(&qualified[index + 1..]);
        }
    }
    None
}

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

    pub(crate) fn clear(&mut self) {
        self.index.clear();
        self.nulls.clear();
        self.has_temporal = false;
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
    pub(crate) fn value_indexable_fields(&self, table: &str) -> StorageBackendResult<Vec<String>> {
        let Some(table_name) = self.try_resolve_table_name(table)? else {
            return Ok(Vec::new());
        };
        let mut fields = Vec::new();
        if let Some(t) = self.try_table(&table_name)? {
            for column in t.columns.read().iter() {
                if (column.primary_key || column.unique) && !fields.contains(&column.name) {
                    fields.push(column.name.clone());
                }
            }
            for constraint in t.key_constraints.read().iter() {
                for column in &constraint.columns {
                    if !fields.contains(column) {
                        fields.push(column.clone());
                    }
                }
            }
        }
        for row in self.catalog_indexes.read().values() {
            if !row.index_type.eq_ignore_ascii_case("btree") {
                continue;
            }
            if row.table_name != table_name {
                continue;
            }
            let columns: Vec<String> = serde_json::from_str(&row.columns_json)?;
            if let Some(first) = columns.first() {
                if !fields.contains(first) {
                    fields.push(first.clone());
                }
            }
        }
        Ok(fields)
    }

    /// Resolve a scalar predicate on `field` through a value index.
    /// Returns `None` when the column has no index policy, the index
    /// cannot reproduce scan semantics, or the table is unknown.
    pub(crate) fn value_index_scan(
        &self,
        table: &str,
        field: &str,
        predicate: &Predicate,
    ) -> Result<Option<PostingList>, SQLError> {
        let t = self.require_table(table)?;
        {
            let indexes = t.value_indexes.read();
            if let Some(index) = indexes.get(field) {
                return Ok(index.scan(predicate));
            }
        }
        if !self
            .ensure_value_index(table, field)
            .map_err(|error| SQLError::Internal(format!("build value index: {error}")))?
        {
            return Ok(None);
        }
        let result = t
            .value_indexes
            .read()
            .get(field)
            .and_then(|index| index.scan(predicate));
        Ok(result)
    }

    /// Hydrate one value index from durable postings when available. A missing
    /// durable marker is satisfied by an in-memory build only; query execution
    /// must not turn a deferred read transaction into a writer.
    fn ensure_value_index(&self, table: &str, field: &str) -> StorageBackendResult<bool> {
        self.ensure_value_index_with_mode(table, field, MissingValueIndexMode::MemoryOnly)
    }

    /// DDL/open-repair counterpart of [`Engine::ensure_value_index`].
    fn ensure_persistent_value_index(
        &self,
        table: &str,
        field: &str,
    ) -> StorageBackendResult<bool> {
        self.ensure_value_index_with_mode(table, field, MissingValueIndexMode::Persist)
    }

    fn ensure_value_index_with_mode(
        &self,
        table: &str,
        field: &str,
        mode: MissingValueIndexMode,
    ) -> StorageBackendResult<bool> {
        let Some(table_name) = self.try_resolve_table_name(table)? else {
            return Ok(false);
        };
        let Some(t) = self.try_table(&table_name)? else {
            return Ok(false);
        };
        let memory_index_exists = t.value_indexes.read().contains_key(field);
        if !self
            .value_indexable_fields(&table_name)?
            .iter()
            .any(|name| name == field)
        {
            return Ok(false);
        }

        let store = t.document_store.read();
        let persistent_backend = self
            .backend
            .as_ref()
            .filter(|backend| backend.persists_btree_indexes());
        let persisted = persistent_backend
            .map(|backend| backend.load_btree_index(&table_name, field))
            .transpose()?
            .flatten();
        let durable_index_missing = persistent_backend.is_some() && persisted.is_none();
        if memory_index_exists
            && (mode == MissingValueIndexMode::MemoryOnly || !durable_index_missing)
        {
            return Ok(true);
        }
        let values = if let Some(values) = persisted {
            values
        } else {
            let doc_ids = store.doc_ids()?;
            let mut projected = store.get_fields_multi(&doc_ids, &[field])?;
            let mut values = Vec::with_capacity(doc_ids.len());
            for doc_id in doc_ids {
                // `DocumentStore::get_fields_multi` deliberately omits ids
                // without a backing document, so a concurrently removed/stale
                // id may be skipped. An id whose document still exists but was
                // lost from the projection is a broken storage response and
                // must fail the rebuild instead of silently omitting an index
                // entry.
                let Some(row) = projected.remove(&doc_id) else {
                    if store.get(doc_id)?.is_none() {
                        continue;
                    }
                    return Err(StorageBackendError::Other(format!(
                        "value-index rebuild for `{table_name}`.`{field}` lost document {doc_id} from the field projection"
                    )));
                };
                let [value]: [Value; 1] = row.try_into().map_err(|row: Vec<Value>| {
                    StorageBackendError::Other(format!(
                        "value-index rebuild for `{table_name}`.`{field}` returned {} projected values for document {doc_id}; expected 1",
                        row.len()
                    ))
                })?;
                values.push((doc_id, value));
            }
            if mode == MissingValueIndexMode::Persist {
                if let Some(backend) = persistent_backend {
                    backend.replace_btree_index(&table_name, field, &values)?;
                }
            }
            values
        };
        if !memory_index_exists {
            let built = ColumnValueIndex::build(field, values.into_iter());
            let mut indexes = t.value_indexes.write();
            indexes.entry(field.to_string()).or_insert(built);
        }
        Ok(true)
    }

    /// Reconcile one table's in-memory and durable indexes with its current
    /// PRIMARY KEY / UNIQUE / catalog-btree policy.
    pub(crate) fn refresh_value_indexes_for_table(&self, table: &str) -> StorageBackendResult<()> {
        let table_name = self
            .try_resolve_table_name(table)?
            .ok_or_else(|| StorageBackendError::Other(format!("table `{table}` does not exist")))?;
        let t = self.try_table(&table_name)?.ok_or_else(|| {
            StorageBackendError::Other(format!("table `{table_name}` does not exist"))
        })?;
        let desired = self.value_indexable_fields(&table_name)?;
        let mut stale: Vec<String> = t
            .value_indexes
            .read()
            .keys()
            .filter(|field| !desired.contains(field))
            .cloned()
            .collect();
        if let Some(backend) = self
            .backend
            .as_ref()
            .filter(|backend| backend.persists_btree_indexes())
        {
            for field in backend.btree_index_fields(&table_name)? {
                if !desired.contains(&field) && !stale.contains(&field) {
                    stale.push(field);
                }
            }
            for field in &stale {
                backend.drop_btree_index(&table_name, field)?;
            }
        }
        t.value_indexes
            .write()
            .retain(|field, _| desired.contains(field));
        for field in desired {
            self.ensure_persistent_value_index(&table_name, &field)?;
        }
        Ok(())
    }

    /// Reconcile durable value indexes at the explicit database-open repair
    /// boundary. A read-only preflight keeps the normal open/session path out
    /// of `SQLite`'s single-writer lane. Only an observed missing/stale marker or
    /// pre-canonicalization alias opens the writer transaction, where the plan
    /// is recomputed against the pinned snapshot before making any changes.
    pub(crate) fn repair_persistent_value_indexes_on_open(&self) -> StorageBackendResult<()> {
        if self.persistent_value_index_repair_plan()?.is_empty() {
            return Ok(());
        }
        self.with_implicit_storage_transaction(|engine| {
            // Waiting for the writer reservation may have made the preflight
            // stale. Recompute after the transaction has refreshed its pinned
            // catalog/data snapshot and mutate only what is still divergent.
            let plan = engine.persistent_value_index_repair_plan()?;
            let Some(backend) = engine
                .backend
                .as_ref()
                .filter(|backend| backend.persists_btree_indexes())
            else {
                return Ok(());
            };
            for alias in plan.aliases {
                for field in backend.btree_index_fields(&alias)? {
                    backend.drop_btree_index(&alias, &field)?;
                }
            }
            for table in plan.tables {
                engine.refresh_value_indexes_for_table(&table)?;
            }
            Ok(())
        })
    }

    fn persistent_value_index_repair_plan(
        &self,
    ) -> StorageBackendResult<PersistentValueIndexRepairPlan> {
        let Some(backend) = self
            .backend
            .as_ref()
            .filter(|backend| backend.persists_btree_indexes())
        else {
            return Ok(PersistentValueIndexRepairPlan::default());
        };

        let mut plan = PersistentValueIndexRepairPlan::default();
        for table in self.table_names()? {
            let desired: BTreeSet<String> =
                self.value_indexable_fields(&table)?.into_iter().collect();
            let actual: BTreeSet<String> =
                backend.btree_index_fields(&table)?.into_iter().collect();
            let mut has_legacy_alias = false;
            if let Some(alias) = unqualified_relation_key(&table) {
                if !backend.btree_index_fields(alias)?.is_empty() {
                    has_legacy_alias = true;
                    plan.aliases.insert(alias.to_string());
                }
            }
            if actual != desired || has_legacy_alias {
                plan.tables.insert(table);
            }
        }
        Ok(plan)
    }

    /// A persistent rollback reverts `SQLite` postings but not the in-memory
    /// B-tree. Rehydrate only indexes that were already hot, preserving the
    /// lazy-load contract for every other indexed column.
    pub(crate) fn reload_persistent_value_indexes(&self) -> StorageBackendResult<()> {
        if !self
            .backend
            .as_ref()
            .is_some_and(|backend| backend.persists_btree_indexes())
        {
            return Ok(());
        }
        for table in self.table_names()? {
            let Some(t) = self.try_table(&table)? else {
                continue;
            };
            let fields: Vec<String> = t.value_indexes.read().keys().cloned().collect();
            t.value_indexes.write().clear();
            for field in fields {
                self.ensure_value_index(&table, &field)?;
            }
        }
        Ok(())
    }

    /// Values of every logical btree field in a complete document. Persistent
    /// storage ignores fields whose durable posting set has not been repaired
    /// yet; query-time memory indexes remain independent of durable postings.
    pub(crate) fn persistent_value_index_document_values(
        &self,
        table: &str,
        document: &BTreeMap<String, Value>,
    ) -> Result<Option<BTreeMap<String, Value>>, SQLError> {
        if !self
            .backend
            .as_ref()
            .is_some_and(|backend| backend.persists_btree_indexes())
        {
            return Ok(None);
        }
        let table_name = self
            .try_resolve_table_name(table)
            .map_err(|err| SQLError::Internal(format!("resolve value-index table: {err}")))?
            .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
        let fields = self
            .value_indexable_fields(&table_name)
            .map_err(|err| SQLError::Internal(format!("read value-index policy: {err}")))?;
        Ok(Some(
            fields
                .into_iter()
                .map(|field| {
                    let value = document.get(&field).cloned().unwrap_or(Value::Null);
                    (field, value)
                })
                .collect(),
        ))
    }

    pub(crate) fn persist_value_indexes_apply_write(
        &self,
        table: &str,
        doc_id: DocId,
        new: Option<&BTreeMap<String, Value>>,
    ) -> Result<(), SQLError> {
        let Some(backend) = self
            .backend
            .as_ref()
            .filter(|backend| backend.persists_btree_indexes())
        else {
            return Ok(());
        };
        let table_name = self
            .try_resolve_table_name(table)
            .map_err(|err| SQLError::Internal(format!("resolve value-index table: {err}")))?
            .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
        backend
            .apply_btree_index_write(&table_name, doc_id, new)
            .map_err(|err| SQLError::Internal(format!("btree index write failed: {err}")))
    }

    /// TRUNCATE keeps index definitions installed but removes all postings.
    pub(crate) fn value_indexes_truncate(
        &self,
        table: &str,
        t: &TableState,
    ) -> Result<(), SQLError> {
        let table_name = self
            .try_resolve_table_name(table)
            .map_err(|err| SQLError::Internal(format!("resolve value-index table: {err}")))?
            .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
        if let Some(backend) = self
            .backend
            .as_ref()
            .filter(|backend| backend.persists_btree_indexes())
        {
            backend
                .clear_btree_indexes(&table_name)
                .map_err(|err| SQLError::Internal(format!("btree truncate failed: {err}")))?;
        }
        for index in t.value_indexes.write().values_mut() {
            index.clear();
        }
        Ok(())
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

    /// Names of every built value-index field, or `None` when no index
    /// is built. Known-new writes use this instead of
    /// [`Engine::value_indexes_old_values`], because a document id that
    /// was never stored has no previous values worth a storage lookup.
    pub(crate) fn value_indexes_built_fields(t: &TableState) -> Option<Vec<String>> {
        let indexes = t.value_indexes.read();
        if indexes.is_empty() {
            return None;
        }
        Some(indexes.keys().cloned().collect())
    }

    /// Fetch the previous values of every built-index field for
    /// `doc_id`, so a write can unindex them. Returns `None` when no
    /// indexes are built (the common case, costing one read-lock).
    pub(crate) fn value_indexes_old_values(
        t: &TableState,
        doc_id: DocId,
    ) -> Result<Option<BTreeMap<String, Value>>, SQLError> {
        let fields: Vec<String> = {
            let indexes = t.value_indexes.read();
            if indexes.is_empty() {
                return Ok(None);
            }
            indexes.keys().cloned().collect()
        };
        let field_refs: Vec<&str> = fields.iter().map(String::as_str).collect();
        let mut rows = t
            .document_store
            .read()
            .get_fields_multi(&[doc_id], &field_refs)
            .map_err(|error| SQLError::Internal(format!("read indexed fields: {error}")))?;
        let values = rows
            .remove(&doc_id)
            .unwrap_or_else(|| vec![Value::Null; fields.len()]);
        Ok(Some(fields.into_iter().zip(values).collect()))
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
    use std::sync::Arc;
    use uqa_storage::document_store::{Document, DocumentStore};

    #[derive(Clone)]
    struct MissingProjectionStore;

    impl DocumentStore for MissingProjectionStore {
        fn put(&mut self, _doc_id: DocId, _document: Document) -> StorageBackendResult<()> {
            Ok(())
        }

        fn get(&self, doc_id: DocId) -> StorageBackendResult<Option<Document>> {
            Ok((doc_id == 1).then(Document::new))
        }

        fn delete(&mut self, _doc_id: DocId) -> StorageBackendResult<()> {
            Ok(())
        }

        fn clear(&mut self) -> StorageBackendResult<()> {
            Ok(())
        }

        fn get_fields_multi(
            &self,
            _doc_ids: &[DocId],
            _fields: &[&str],
        ) -> StorageBackendResult<BTreeMap<DocId, Vec<Value>>> {
            Ok(BTreeMap::new())
        }

        fn doc_ids(&self) -> StorageBackendResult<Vec<DocId>> {
            Ok(vec![1])
        }

        fn len(&self) -> StorageBackendResult<usize> {
            Ok(1)
        }

        fn snapshot(&self) -> StorageBackendResult<Arc<dyn DocumentStore>> {
            Ok(Arc::new(self.clone()))
        }

        fn writable_snapshot(&self) -> StorageBackendResult<Box<dyn DocumentStore>> {
            Ok(Box::new(self.clone()))
        }
    }

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

    #[test]
    fn rebuild_rejects_a_document_missing_from_the_field_projection() {
        let engine = crate::Engine::new();
        engine
            .sql("CREATE TABLE projection_gap (id INTEGER PRIMARY KEY)", &[])
            .unwrap();
        let table = engine.try_table("projection_gap").unwrap().unwrap();
        *table.document_store.write() = Box::new(MissingProjectionStore);
        crate::Engine::value_indexes_clear(&table);

        let error = engine
            .ensure_value_index("projection_gap", "id")
            .unwrap_err();
        assert!(error.to_string().contains("lost document 1"), "{error}");
        assert!(table.value_indexes.read().is_empty());
    }

    #[test]
    fn relation_key_suffix_preserves_quoted_components() {
        assert_eq!(unqualified_relation_key("public.items"), Some("items"));
        assert_eq!(
            unqualified_relation_key("public.\"items.with.dot\""),
            Some("\"items.with.dot\"")
        );
        assert_eq!(
            unqualified_relation_key("\"schema.with.dot\".\"items.with.dot\""),
            Some("\"items.with.dot\"")
        );
        assert_eq!(
            unqualified_relation_key("public.\"items\"\"quoted\""),
            Some("\"items\"\"quoted\"")
        );
    }

    #[test]
    fn query_builds_missing_durable_index_in_memory_only() {
        let directory = tempfile::tempdir().unwrap();
        let engine = crate::Engine::open(&directory.path().join("memory-only-btree.db")).unwrap();
        engine
            .sql("CREATE TABLE items (id INTEGER PRIMARY KEY)", &[])
            .unwrap();
        engine
            .sql("INSERT INTO items (id) VALUES (1)", &[])
            .unwrap();
        let backend = engine.backend.as_ref().unwrap();
        backend.drop_btree_index("public.items", "id").unwrap();
        let table = engine.try_table("items").unwrap().unwrap();
        crate::Engine::value_indexes_clear(&table);

        let result = engine
            .sql("SELECT id FROM items WHERE id = 1", &[])
            .unwrap();
        assert_eq!(result.rows.len(), 1);
        assert!(backend
            .load_btree_index("public.items", "id")
            .unwrap()
            .is_none());
        assert!(engine
            .try_table("items")
            .unwrap()
            .unwrap()
            .value_indexes
            .read()
            .contains_key("id"));

        // Rollback recovery clears hot indexes before hydrating them again;
        // a missing durable marker must remain a memory-only cache miss there
        // too, rather than silently turning rollback into a new write.
        engine.reload_persistent_value_indexes().unwrap();
        assert!(backend
            .load_btree_index("public.items", "id")
            .unwrap()
            .is_none());
        assert!(engine
            .try_table("items")
            .unwrap()
            .unwrap()
            .value_indexes
            .read()
            .contains_key("id"));

        // The explicit persistence path must not mistake the hot memory cache
        // for a durable marker.
        engine.ensure_persistent_value_index("items", "id").unwrap();
        assert_eq!(
            backend
                .load_btree_index("public.items", "id")
                .unwrap()
                .unwrap(),
            vec![(1, Value::Int(1))]
        );
    }

    #[test]
    fn open_repair_discards_raw_alias_and_rebuilds_canonical_index() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("repair-btree.db");
        let engine = crate::Engine::open(&database).unwrap();
        engine
            .sql("CREATE TABLE items (id INTEGER PRIMARY KEY)", &[])
            .unwrap();
        engine
            .sql("INSERT INTO items (id) VALUES (1)", &[])
            .unwrap();
        let backend = engine.backend.as_ref().unwrap().clone();
        backend.drop_btree_index("public.items", "id").unwrap();
        backend
            .replace_btree_index("public.items", "obsolete", &[(1, Value::Int(888))])
            .unwrap();
        backend
            .replace_btree_index("items", "id", &[(1, Value::Int(999))])
            .unwrap();
        let table = engine.try_table("items").unwrap().unwrap();
        crate::Engine::value_indexes_clear(&table);
        drop(table);
        drop(backend);
        drop(engine);

        let reopened = crate::Engine::open(&database).unwrap();
        let backend = reopened.backend.as_ref().unwrap();

        assert!(backend.load_btree_index("items", "id").unwrap().is_none());
        assert!(backend
            .load_btree_index("public.items", "obsolete")
            .unwrap()
            .is_none());
        assert_eq!(
            backend
                .load_btree_index("public.items", "id")
                .unwrap()
                .unwrap(),
            vec![(1, Value::Int(1))]
        );
    }

    #[test]
    fn clean_open_repair_does_not_contend_for_sqlite_writer_lock() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("clean-repair.db");
        let engine = crate::Engine::open(&database).unwrap();
        engine
            .sql("CREATE TABLE items (id INTEGER PRIMARY KEY)", &[])
            .unwrap();
        engine
            .sql("INSERT INTO items (id) VALUES (1)", &[])
            .unwrap();
        assert!(engine
            .persistent_value_index_repair_plan()
            .unwrap()
            .is_empty());

        // A clean repair is read-only and therefore succeeds while an
        // independent session owns SQLite's single writer reservation. If the
        // repair unconditionally issued BEGIN IMMEDIATE this would block and
        // eventually return SQLITE_BUSY.
        let blocker = engine.sqlite_session.as_ref().unwrap().new_session();
        blocker.begin_transaction().unwrap();
        let repair_result = engine.repair_persistent_value_indexes_on_open();
        let new_session_result = engine.new_session();
        let reopen_result = crate::Engine::open(&database);
        blocker.rollback_transaction().unwrap();
        repair_result.unwrap();
        new_session_result.unwrap();
        reopen_result.unwrap();
    }
}
