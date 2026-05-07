//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Index manager: registry that creates / drops / looks up indexes.
//!
//! Mirrors `uqa/storage/index_manager.py`. Owns the in-memory map of
//! `Box<dyn Index>` and resolves `find_covering_index` lookups for
//! the planner. The Rust port keeps the registry in memory and
//! delegates persistence to the catalog when wired by the engine.

#![allow(clippy::needless_pass_by_value, clippy::map_unwrap_or, unused_imports)]

use std::collections::BTreeMap;
use std::sync::Mutex;

use uqa_core::{Predicate, Value};

use crate::btree_index::BTreeIndex;
use crate::index_abc::Index;
use crate::index_types::{IndexDef, IndexType};
use crate::sqlite::connection::ManagedConnection;
use crate::SQLiteError;

/// Thin [`Index`] adapter over an in-memory [`BTreeIndex`]. Each
/// adapter owns its [`IndexDef`] so the registry can route lookups
/// by table/column.
pub struct BTreeIndexHandle {
    def: IndexDef,
    inner: BTreeIndex,
}

impl BTreeIndexHandle {
    pub fn new(def: IndexDef) -> Self {
        let field = def.columns.first().cloned().unwrap_or_default();
        Self {
            def,
            inner: BTreeIndex::new(field),
        }
    }

    pub fn insert(&mut self, doc_id: u64, value: Value) {
        self.inner.insert(doc_id, value);
    }

    pub fn remove(&mut self, doc_id: u64, value: &Value) {
        self.inner.remove(doc_id, value);
    }

    pub fn clear(&mut self) {
        self.inner.clear();
    }

    pub fn inner(&self) -> &BTreeIndex {
        &self.inner
    }

    pub fn inner_mut(&mut self) -> &mut BTreeIndex {
        &mut self.inner
    }
}

impl Index for BTreeIndexHandle {
    fn index_def(&self) -> &IndexDef {
        &self.def
    }
    fn scan(&self, predicate: &Predicate) -> uqa_core::PostingList {
        self.inner.scan(predicate)
    }
    fn estimate_cardinality(&self, predicate: &Predicate) -> u64 {
        self.inner.scan(predicate).len() as u64
    }
    fn scan_cost(&self, predicate: &Predicate) -> f64 {
        // Cost proxy: equality predicates are cheap (one bucket lookup);
        // ranges scale with the predicted matching cardinality. The
        // planner reads relative numbers so the absolute scale is
        // unimportant.
        let card = self.estimate_cardinality(predicate) as f64;
        match predicate {
            Predicate::Equals(_) => 1.0 + card * 0.1,
            _ => card.max(1.0),
        }
    }
    fn build(&mut self) -> Result<(), SQLiteError> {
        Ok(())
    }
    fn drop_index(&mut self) -> Result<(), SQLiteError> {
        self.inner.clear();
        Ok(())
    }
}

/// Index registry. Constructed once per [`crate::Catalog`] and shared
/// across the engine's tables.
pub struct IndexManager {
    #[allow(dead_code)]
    conn: ManagedConnection,
    indexes: Mutex<BTreeMap<String, Box<dyn Index>>>,
}

impl IndexManager {
    pub fn new(conn: ManagedConnection) -> Self {
        Self {
            conn,
            indexes: Mutex::new(BTreeMap::new()),
        }
    }

    /// Build a physical index and register the definition under
    /// `index_def.name`. Returns an error if an index with the same
    /// name is already registered.
    pub fn create_index(&self, index_def: IndexDef) -> Result<(), SQLiteError> {
        let mut guard = self.indexes.lock().unwrap();
        if guard.contains_key(&index_def.name) {
            return Err(SQLiteError::SQLite(rusqlite::Error::QueryReturnedNoRows));
        }
        let mut index: Box<dyn Index> = match index_def.index_type {
            IndexType::BTree => Box::new(BTreeIndexHandle::new(index_def.clone())),
            other => {
                let _ = other;
                // Other index types live in their own modules; the
                // engine wires them at table-creation time and the
                // manager only owns the registry entry.
                Box::new(BTreeIndexHandle::new(index_def.clone()))
            }
        };
        index.build()?;
        guard.insert(index_def.name.clone(), index);
        Ok(())
    }

    pub fn drop_index(&self, name: &str) -> Result<bool, SQLiteError> {
        let mut guard = self.indexes.lock().unwrap();
        if let Some(mut idx) = guard.remove(name) {
            idx.drop_index()?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    pub fn drop_indexes_for_table(&self, table_name: &str) -> Result<(), SQLiteError> {
        let mut guard = self.indexes.lock().unwrap();
        let names: Vec<String> = guard
            .iter()
            .filter(|(_, idx)| idx.index_def().table_name == table_name)
            .map(|(n, _)| n.clone())
            .collect();
        for name in names {
            if let Some(mut idx) = guard.remove(&name) {
                idx.drop_index()?;
            }
        }
        Ok(())
    }

    pub fn find_covering_index_name(
        &self,
        table_name: &str,
        column: &str,
        predicate: &Predicate,
    ) -> Option<String> {
        self.find_covering_index_with_cost(table_name, column, predicate)
            .map(|(name, _)| name)
    }

    /// Like [`Self::find_covering_index_name`] but returns the chosen
    /// index's name together with its `scan_cost(predicate)` so the
    /// caller can compare against a full-scan cost before committing
    /// to the rewrite. Mirrors Python's `_apply_index_scan`
    /// `scan_cost < full_scan_cost` gate.
    pub fn find_covering_index_with_cost(
        &self,
        table_name: &str,
        column: &str,
        predicate: &Predicate,
    ) -> Option<(String, f64)> {
        let guard = self.indexes.lock().unwrap();
        let mut best: Option<(String, f64)> = None;
        for (name, idx) in guard.iter() {
            let def = idx.index_def();
            if def.table_name != table_name {
                continue;
            }
            if def.columns.first().map(String::as_str) != Some(column) {
                continue;
            }
            let cost = idx.scan_cost(predicate);
            if best.as_ref().map(|(_, c)| cost < *c).unwrap_or(true) {
                best = Some((name.clone(), cost));
            }
        }
        best
    }

    pub fn has_index(&self, name: &str) -> bool {
        self.indexes.lock().unwrap().contains_key(name)
    }

    pub fn index_count(&self) -> usize {
        self.indexes.lock().unwrap().len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh() -> IndexManager {
        let conn = ManagedConnection::open_in_memory().unwrap();
        IndexManager::new(conn)
    }

    #[test]
    fn fresh_manager_has_no_indexes() {
        let mgr = fresh();
        assert_eq!(mgr.index_count(), 0);
        assert!(!mgr.has_index("missing"));
    }

    #[test]
    fn drop_unknown_index_is_noop() {
        let mgr = fresh();
        assert!(!mgr.drop_index("missing").unwrap());
    }

    #[test]
    fn create_then_drop_btree_index_reflects_in_count() {
        let mgr = fresh();
        let def = IndexDef::new(
            "users_age_idx",
            IndexType::BTree,
            "users",
            vec!["age".into()],
        );
        mgr.create_index(def).unwrap();
        assert_eq!(mgr.index_count(), 1);
        assert!(mgr.has_index("users_age_idx"));
        assert!(mgr.drop_index("users_age_idx").unwrap());
        assert_eq!(mgr.index_count(), 0);
    }

    #[test]
    fn find_covering_index_picks_matching_btree() {
        let mgr = fresh();
        mgr.create_index(IndexDef::new(
            "users_age_idx",
            IndexType::BTree,
            "users",
            vec!["age".into()],
        ))
        .unwrap();
        let pred = Predicate::Equals(Value::Int(42));
        let pick = mgr.find_covering_index_name("users", "age", &pred);
        assert_eq!(pick, Some("users_age_idx".into()));
    }
}
