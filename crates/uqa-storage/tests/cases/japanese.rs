//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::sync::Arc;
use uqa_analysis::whitespace_analyzer;
use uqa_storage::{InvertedIndex, KeyValueInvertedIndex, MemoryInvertedIndex, MemoryKeyValueStore};

#[path = "japanese/contract.rs"]
mod contract;

#[test]
fn japanese_occurrences_preserve_reference_graphs_and_revisions_in_memory_and_key_value() {
    for case in contract::cases() {
        let mut memory = MemoryInvertedIndex::new(whitespace_analyzer());
        let store = Arc::new(MemoryKeyValueStore::new());
        let mut persistent =
            KeyValueInvertedIndex::new(store.clone(), "docs", whitespace_analyzer());
        for index in [&mut memory as &mut dyn InvertedIndex, &mut persistent] {
            contract::populate(index, &case);
            contract::verify(index, &case);
            contract::restore(index, &case);
            contract::verify(index, &case);
            assert_eq!(
                index
                    .search_analyzer_revision("body")
                    .unwrap()
                    .analyze(&case.input)
                    .unwrap(),
                [case.input.as_str()]
            );
        }
        drop(persistent);
        let mut reopened = KeyValueInvertedIndex::new(store, "docs", whitespace_analyzer());
        contract::restore(&mut reopened, &case);
        contract::verify(&reopened, &case);
        let snapshot = memory.snapshot().unwrap();
        memory.remove_document(7).unwrap();
        contract::verify(snapshot.as_ref(), &case);
    }
}
