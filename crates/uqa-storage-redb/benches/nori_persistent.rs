//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! redb Nori batch commits and rollbacks with reopened occurrence verification.

use std::path::Path;
use std::sync::Arc;
use uqa_storage::{KeyValueInvertedIndex, KeyValueStore};
use uqa_storage_redb::{RedbKeyValueStore, RedbStorage};

#[path = "../../../benchmarks/nori/persistent.rs"]
mod persistent;

struct Session {
    index: KeyValueInvertedIndex,
    store: Arc<RedbKeyValueStore>,
}

impl persistent::Session for Session {
    type Index = KeyValueInvertedIndex;
    const TRANSACTION_MODEL: &'static str = "versioned_concurrent";

    fn open(path: &Path) -> Self {
        let provider = RedbStorage::open(path).unwrap();
        let store = Arc::new(provider.store());
        assert!(store.transaction_model().is_versioned());
        let index =
            KeyValueInvertedIndex::new(store.clone(), "docs", uqa_analysis::whitespace_analyzer());
        Self { index, store }
    }

    fn index(&mut self) -> &mut Self::Index {
        &mut self.index
    }

    fn begin(&self) {
        self.store.begin_transaction().unwrap();
    }

    fn finish(&self, rollback: bool) {
        if rollback {
            self.store.rollback_transaction().unwrap();
        } else {
            self.store.commit_transaction().unwrap();
        }
    }

    fn retained_transaction_bytes(&self) -> Option<usize> {
        Some(self.store.retention_control().memory().used())
    }
}

fn main() {
    persistent::run::<Session>(
        "uqa-storage-redb",
        "redb default immediate commit durability",
    );
}
