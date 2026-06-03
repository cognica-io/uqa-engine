//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    BTreeMap, CatalogFacade, Engine, SequenceState, StorageBackendResult, SEQUENCES_METADATA_KEY,
};

impl Engine {
    pub fn create_sequence(
        &self,
        name: &str,
        start: i64,
        increment: i64,
        if_not_exists: bool,
    ) -> bool {
        let name = self.relation_name_for_create(name);
        let mut seqs = self.sequences.write();
        if seqs.contains_key(&name) {
            return if_not_exists;
        }
        seqs.insert(
            name,
            SequenceState {
                start,
                increment,
                current: start - increment,
            },
        );
        drop(seqs);
        self.persist_sequences();
        true
    }

    pub fn alter_sequence(
        &self,
        name: &str,
        restart: Option<Option<i64>>,
        increment: Option<i64>,
        start: Option<i64>,
    ) -> Result<(), String> {
        let name = self
            .resolve_sequence_name(name)
            .ok_or_else(|| format!("Sequence `{name}` does not exist"))?;
        let mut seqs = self.sequences.write();
        let seq = seqs
            .get_mut(&name)
            .ok_or_else(|| format!("Sequence `{name}` does not exist"))?;
        if let Some(start_val) = start {
            seq.start = start_val;
        }
        if let Some(inc) = increment {
            seq.increment = inc;
        }
        if let Some(opt) = restart {
            let restart_val = opt.unwrap_or(seq.start);
            seq.current = restart_val - seq.increment;
        }
        drop(seqs);
        self.persist_sequences();
        Ok(())
    }

    pub fn drop_sequence(&self, name: &str) -> bool {
        let Some(name) = self.resolve_sequence_name(name) else {
            return false;
        };
        let removed = self.sequences.write().remove(&name).is_some();
        if removed {
            self.persist_sequences();
        }
        removed
    }

    pub fn nextval(&self, name: &str) -> Result<i64, String> {
        let name = self
            .resolve_sequence_name(name)
            .ok_or_else(|| format!("Sequence `{name}` does not exist"))?;
        let mut seqs = self.sequences.write();
        let seq = seqs
            .get_mut(&name)
            .ok_or_else(|| format!("Sequence `{name}` does not exist"))?;
        seq.current += seq.increment;
        let current = seq.current;
        drop(seqs);
        self.persist_sequences();
        Ok(current)
    }

    pub fn currval(&self, name: &str) -> Result<i64, String> {
        let name = self
            .resolve_sequence_name(name)
            .ok_or_else(|| format!("Sequence `{name}` does not exist"))?;
        let seqs = self.sequences.read();
        seqs.get(&name)
            .map(|s| s.current)
            .ok_or_else(|| format!("Sequence `{name}` does not exist"))
    }

    pub fn setval(&self, name: &str, value: i64) -> Result<i64, String> {
        let name = self
            .resolve_sequence_name(name)
            .ok_or_else(|| format!("Sequence `{name}` does not exist"))?;
        let mut seqs = self.sequences.write();
        let seq = seqs
            .get_mut(&name)
            .ok_or_else(|| format!("Sequence `{name}` does not exist"))?;
        seq.current = value;
        drop(seqs);
        self.persist_sequences();
        Ok(value)
    }

    /// Snapshot of all registered sequences as `(name, state)` pairs.
    pub fn sequences_snapshot(&self) -> BTreeMap<String, SequenceState> {
        self.sequences.read().clone()
    }

    /// Resolve a sequence name through the current `search_path` and return
    /// its canonical name with the current state.
    pub fn sequence_state(&self, name: &str) -> Option<(String, SequenceState)> {
        let canonical = self.resolve_sequence_name(name)?;
        let seqs = self.sequences.read();
        seqs.get(&canonical)
            .copied()
            .map(|state| (canonical, state))
    }

    fn persist_sequences(&self) {
        let Some(catalog) = self.catalog.as_ref() else {
            return;
        };
        if let Ok(json) = serde_json::to_string(&*self.sequences.read()) {
            let _ = catalog.set_metadata(SEQUENCES_METADATA_KEY, &json);
        }
    }

    pub(crate) fn restore_sequences_from_metadata(
        &self,
        catalog: &dyn CatalogFacade,
    ) -> StorageBackendResult<()> {
        let Some(json) = catalog.get_metadata(SEQUENCES_METADATA_KEY)? else {
            return Ok(());
        };
        if let Ok(sequences) = serde_json::from_str::<BTreeMap<String, SequenceState>>(&json) {
            *self.sequences.write() = sequences;
        }
        Ok(())
    }

    // -----------------------------------------------------------------
    // Prepared statements. Mirrors `_engine._prepared`.
    // -----------------------------------------------------------------

    pub fn register_prepared(&self, name: String, stmt: uqa_sql::ast::Statement) {
        self.prepared.write().insert(name, stmt);
    }

    pub fn lookup_prepared(&self, name: &str) -> Option<uqa_sql::ast::Statement> {
        self.prepared.read().get(name).cloned()
    }

    pub fn deallocate_prepared(&self, name: Option<&str>) {
        match name {
            Some(n) => {
                self.prepared.write().remove(n);
            }
            None => self.prepared.write().clear(),
        }
    }
}
