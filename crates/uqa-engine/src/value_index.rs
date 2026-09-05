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
//! then compose through their document-id support like any other signal.
//! Indexes are built on first use from one bulk field scan and
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
use uqa_storage::{BTreeIndex, ValueIndexKey};

mod keys;

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
    /// Durable retry markers written by a catalog migration. They are cleared
    /// in the same transaction, and only after every requested repair succeeds.
    pending: BTreeSet<(String, ValueIndexKey)>,
}

impl PersistentValueIndexRepairPlan {
    fn is_empty(&self) -> bool {
        self.aliases.is_empty() && self.tables.is_empty() && self.pending.is_empty()
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
#[derive(Clone)]
pub(crate) struct ColumnValueIndex {
    index: BTreeIndex,
    values: BTreeMap<DocId, Value>,
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
        let mut stored = BTreeMap::new();
        let mut nulls = Vec::new();
        let mut has_temporal = false;
        for (doc_id, value) in values {
            stored.insert(doc_id, value.clone());
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
            values: stored,
            nulls,
            has_temporal,
        }
    }

    pub(crate) fn insert(&mut self, doc_id: DocId, value: &Value) {
        self.values.insert(doc_id, value.clone());
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

    pub(crate) fn clear(&mut self) {
        self.index.clear();
        self.values.clear();
        self.nulls.clear();
        self.has_temporal = false;
    }

    /// Resolve `predicate` to a posting list, or `None` when this
    /// index cannot reproduce evaluated-scan semantics for it.
    pub(crate) fn scan(&self, predicate: &Predicate) -> Option<PostingList> {
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

    pub(crate) fn estimate_cardinality(&self, predicate: &Predicate) -> Option<usize> {
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

    fn supports(&self, predicate: &Predicate) -> bool {
        predicate_targets_are_index_safe(predicate)
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

impl crate::Engine {
    fn persistent_value_index_backend(
        &self,
        table: &TableState,
    ) -> Option<&dyn uqa_storage::PersistentStorageBackend> {
        if table.persistence == uqa_sql::ast::RelationPersistence::Temporary {
            return None;
        }
        self.storage
            .backend
            .as_deref()
            .filter(|backend| backend.persists_btree_indexes())
    }

    fn value_index_table_is_temporary(&self, table: &str) -> Result<bool, SQLError> {
        self.try_table(table)
            .map_err(|err| SQLError::Internal(format!("resolve value-index table: {err}")))?
            .map(|table| table.persistence == uqa_sql::ast::RelationPersistence::Temporary)
            .ok_or_else(|| SQLError::UnknownTable(table.to_string()))
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
        self.value_index_scan_key(table, &ValueIndexKey::Column(field.into()), predicate)
    }

    pub(crate) fn value_index_scan_key(
        &self,
        table: &str,
        field: &ValueIndexKey,
        predicate: &Predicate,
    ) -> Result<Option<PostingList>, SQLError> {
        let t = self.require_query_table(table)?;
        {
            let indexes = t.value_indexes.read();
            if let Some(index) = indexes.get(field) {
                return Ok(index.scan(predicate));
            }
        }
        if !self
            .ensure_query_value_index(table, &t, field)
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

    /// Estimate one exact value-index predicate without materializing or
    /// sorting its posting list. Engine column indexes keep every document in
    /// one value bucket, so the storage upper bound is exact here.
    pub(crate) fn value_index_cardinality(
        &self,
        table: &str,
        field: &str,
        predicate: &Predicate,
    ) -> Result<Option<usize>, SQLError> {
        let field = &ValueIndexKey::Column(field.into());
        let table_state = self.require_query_table(table)?;
        {
            let indexes = table_state.value_indexes.read();
            if let Some(index) = indexes.get(field) {
                return Ok(index.estimate_cardinality(predicate));
            }
        }
        if !self
            .ensure_query_value_index(table, &table_state, field)
            .map_err(|error| SQLError::Internal(format!("build value index: {error}")))?
        {
            return Ok(None);
        }
        let cardinality = table_state
            .value_indexes
            .read()
            .get(field)
            .and_then(|index| index.estimate_cardinality(predicate));
        Ok(cardinality)
    }

    /// Return whether catalog policy provides an exact in-memory value-index
    /// implementation for this predicate. Missing hot state is hydrated in
    /// memory, preserving the read-only lazy-recovery contract without forcing
    /// the relational planner to execute every scalar filter as a posting scan.
    pub(crate) fn value_index_supports(
        &self,
        table: &str,
        field: &str,
        predicate: &Predicate,
    ) -> StorageBackendResult<bool> {
        let field = &ValueIndexKey::Column(field.into());
        let Some(table_name) = self.try_resolve_table_name(table)? else {
            return Ok(false);
        };
        let Some(table) = self.try_query_table(&table_name)? else {
            return Ok(false);
        };
        if !self.ensure_query_value_index(&table_name, &table, field)? {
            return Ok(false);
        }
        let supported = table
            .value_indexes
            .read()
            .get(field)
            .is_some_and(|index| index.supports(predicate));
        Ok(supported)
    }

    fn ensure_query_value_index(
        &self,
        table_name: &str,
        table: &std::sync::Arc<TableState>,
        field: &ValueIndexKey,
    ) -> StorageBackendResult<bool> {
        if table.value_indexes.read().contains_key(field) {
            return Ok(true);
        }
        if let Some(live) = self.try_table(table_name)? {
            if std::sync::Arc::ptr_eq(&live, table) {
                return self.ensure_value_index(table_name, field);
            }
        }
        if !self
            .value_indexable_fields(table_name)?
            .iter()
            .any(|name| name == field)
        {
            return Ok(false);
        }
        let ids = table.document_store.read().doc_ids()?;
        let values = self.project_value_index_rows(table, table_name, field, &ids)?;
        table.value_indexes.write().insert(
            field.clone(),
            ColumnValueIndex::build(field.name(), values.into_iter()),
        );
        Ok(true)
    }

    /// Hydrate one value index from durable postings when available. A missing
    /// durable marker is satisfied by an in-memory build only; query execution
    /// must not turn a deferred read transaction into a writer.
    fn ensure_value_index(&self, table: &str, field: &ValueIndexKey) -> StorageBackendResult<bool> {
        self.ensure_value_index_with_mode(table, field, MissingValueIndexMode::MemoryOnly)
    }

    /// DDL/open-repair counterpart of [`Engine::ensure_value_index`].
    fn ensure_persistent_value_index(
        &self,
        table: &str,
        field: &ValueIndexKey,
    ) -> StorageBackendResult<bool> {
        self.ensure_value_index_with_mode(table, field, MissingValueIndexMode::Persist)
    }

    fn ensure_value_index_with_mode(
        &self,
        table: &str,
        field: &ValueIndexKey,
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

        let persistent_backend = self.persistent_value_index_backend(&t);
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
        let (values, support_changed, repair_delta) = if let Some(values) = persisted {
            let mut persisted_ids = values.iter().map(|(doc_id, _)| *doc_id).collect::<Vec<_>>();
            persisted_ids.sort_unstable();
            let mut document_ids = t.document_store.read().doc_ids()?;
            document_ids.sort_unstable();
            if persisted_ids == document_ids {
                (values, false, None)
            } else {
                // Keep every posting that still has an authoritative document
                // and parse only documents whose posting is missing. Historical
                // inconsistencies are normally sparse; rebuilding the complete
                // field could otherwise parse gigabytes to repair one row.
                let document_id_set = document_ids.iter().copied().collect::<BTreeSet<_>>();
                let mut present = BTreeSet::new();
                let mut repaired = Vec::with_capacity(document_ids.len());
                let mut stale = Vec::new();
                for (doc_id, value) in values {
                    if document_id_set.contains(&doc_id) {
                        present.insert(doc_id);
                        repaired.push((doc_id, value));
                    } else {
                        stale.push(doc_id);
                    }
                }
                let missing = document_ids
                    .into_iter()
                    .filter(|doc_id| !present.contains(doc_id))
                    .collect::<Vec<_>>();
                let missing = self.project_value_index_rows(&t, &table_name, field, &missing)?;
                repaired.extend(missing.iter().cloned());
                repaired.sort_unstable_by_key(|(doc_id, _)| *doc_id);
                (repaired, true, Some((stale, missing)))
            }
        } else {
            (
                self.project_value_index_rows(&t, &table_name, field, &{
                    let ids = t.document_store.read().doc_ids()?;
                    ids
                })?,
                true,
                None,
            )
        };
        if support_changed && mode == MissingValueIndexMode::Persist {
            if let Some(backend) = persistent_backend {
                if let Some((stale, missing)) = repair_delta.as_ref() {
                    backend.repair_btree_index(&table_name, field, &values, stale, missing)?;
                } else {
                    backend.replace_btree_index(&table_name, field, &values)?;
                }
            }
        }
        if !memory_index_exists || support_changed {
            let built = ColumnValueIndex::build(field.name(), values.into_iter());
            let mut indexes = t.value_indexes.write();
            if support_changed {
                indexes.insert(field.clone(), built);
            } else {
                indexes.entry(field.clone()).or_insert(built);
            }
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
        let mut stale: Vec<ValueIndexKey> = t
            .value_indexes
            .read()
            .keys()
            .filter(|field| !desired.contains(field))
            .cloned()
            .collect();
        let persistent_backend = self.persistent_value_index_backend(&t);
        let mut persisted_fields = BTreeSet::new();
        if let Some(backend) = persistent_backend {
            for field in backend.btree_index_fields(&table_name)? {
                if !desired.contains(&field) && !stale.contains(&field) {
                    stale.push(field);
                } else {
                    persisted_fields.insert(field);
                }
            }
            for field in &stale {
                backend.drop_btree_index(&table_name, field)?;
                persisted_fields.remove(field);
            }
        }
        t.value_indexes
            .write()
            .retain(|field, _| desired.contains(field));

        if let Some(backend) = persistent_backend {
            let missing = desired
                .iter()
                .filter(|field| !persisted_fields.contains(*field))
                .cloned()
                .collect::<Vec<_>>();
            self.rebuild_persistent_value_indexes(&table_name, &t, &missing, backend)?;
            for field in desired
                .iter()
                .filter(|field| persisted_fields.contains(*field))
            {
                self.ensure_persistent_value_index(&table_name, field)?;
            }
        } else {
            for field in desired {
                self.ensure_persistent_value_index(&table_name, &field)?;
            }
        }
        Ok(())
    }

    /// Reconcile durable value indexes at the explicit database-open repair
    /// boundary. A read-only preflight keeps the normal open/session path out
    /// of `SQLite`'s single-writer lane. Only an observed missing/stale marker,
    /// pending structural repair, or pre-canonicalization alias opens the writer
    /// transaction, where the plan is recomputed against the pinned snapshot
    /// before making any changes.
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
                .storage
                .backend
                .as_ref()
                .filter(|backend| backend.persists_btree_indexes())
            else {
                return Ok(());
            };
            for alias in &plan.aliases {
                for field in backend.btree_index_fields(alias)? {
                    backend.drop_btree_index(alias, &field)?;
                }
            }
            for table in &plan.tables {
                engine.refresh_value_indexes_for_table(table)?;
            }
            for (table, field) in &plan.pending {
                if !plan.tables.contains(table) {
                    let should_exist = engine.try_table(table)?.is_some()
                        && engine
                            .value_indexable_fields(table)?
                            .iter()
                            .any(|candidate| candidate == field);
                    if should_exist {
                        engine.ensure_persistent_value_index(table, field)?;
                    } else {
                        backend.drop_btree_index(table, field)?;
                    }
                }
                backend.clear_btree_index_repair(table, field)?;
            }
            Ok(())
        })
    }

    fn persistent_value_index_repair_plan(
        &self,
    ) -> StorageBackendResult<PersistentValueIndexRepairPlan> {
        let Some(backend) = self
            .storage
            .backend
            .as_ref()
            .filter(|backend| backend.persists_btree_indexes())
        else {
            return Ok(PersistentValueIndexRepairPlan::default());
        };

        let mut plan = PersistentValueIndexRepairPlan {
            pending: backend.btree_index_repairs()?.into_iter().collect(),
            ..PersistentValueIndexRepairPlan::default()
        };
        for table in self.table_names()? {
            let desired: BTreeSet<ValueIndexKey> =
                self.value_indexable_fields(&table)?.into_iter().collect();
            let actual: BTreeSet<ValueIndexKey> =
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

    /// Restore hot accelerators directly from rolled-back postings. Recovery can hold the transaction mutex, so it must never bind SQL expressions or execute callbacks; missing indexes remain cold until the next statement.
    pub(crate) fn reload_persistent_value_indexes(&self) -> StorageBackendResult<()> {
        let Some(backend) = self
            .storage
            .backend
            .as_ref()
            .filter(|backend| backend.persists_btree_indexes())
        else {
            return Ok(());
        };
        let tables = self
            .storage
            .tables
            .read()
            .iter()
            .map(|(name, table)| (name.qualified_name(), table.clone()))
            .collect::<Vec<_>>();
        for (name, table) in tables {
            if table.persistence == uqa_sql::ast::RelationPersistence::Temporary {
                continue;
            }
            let fields = table
                .value_indexes
                .read()
                .keys()
                .cloned()
                .collect::<Vec<_>>();
            table.value_indexes.write().clear();
            for field in fields {
                if let Some(values) = backend.load_btree_index(&name, &field)? {
                    let index = ColumnValueIndex::build(field.name(), values.into_iter());
                    table.value_indexes.write().insert(field, index);
                }
            }
        }
        Ok(())
    }

    pub(crate) fn persist_value_indexes_apply_write(
        &self,
        table: &str,
        doc_id: DocId,
        new: Option<&BTreeMap<ValueIndexKey, Value>>,
    ) -> Result<(), SQLError> {
        let Some(backend) = self
            .storage
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
        if self.value_index_table_is_temporary(&table_name)? {
            return Ok(());
        }
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
        if let Some(backend) = self.persistent_value_index_backend(t) {
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
        old: Option<&BTreeMap<ValueIndexKey, Value>>,
        new: Option<&BTreeMap<ValueIndexKey, Value>>,
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
    pub(crate) fn value_indexes_built_fields(t: &TableState) -> Option<Vec<ValueIndexKey>> {
        let indexes = t.value_indexes.read();
        if indexes.is_empty() {
            return None;
        }
        Some(indexes.keys().cloned().collect())
    }

    /// Read the actual cached keys, which may differ from re-evaluating a replaced immutable function on the old document.
    pub(crate) fn value_indexes_old_values(
        t: &TableState,
        doc_id: DocId,
    ) -> Option<BTreeMap<ValueIndexKey, Value>> {
        let indexes = t.value_indexes.read();
        (!indexes.is_empty()).then(|| {
            indexes
                .iter()
                .map(|(field, index)| {
                    (
                        field.clone(),
                        index.values.get(&doc_id).cloned().unwrap_or(Value::Null),
                    )
                })
                .collect()
        })
    }

    /// Drop every built index for the table (TRUNCATE, bulk reloads,
    /// store replacement, schema changes).
    pub(crate) fn value_indexes_clear(t: &TableState) {
        t.value_indexes.write().clear();
    }

    /// Named memory indexes own evaluated SQL keys; data-epoch invalidation may discard only reconstructible column accelerators.
    pub(crate) fn value_indexes_clear_column_accelerators(t: &TableState) {
        t.value_indexes
            .write()
            .retain(|key, _| matches!(key, ValueIndexKey::Index(_)));
    }
}

#[cfg(test)]
mod tests;
