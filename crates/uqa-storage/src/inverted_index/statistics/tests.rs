//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    key_value::{KeyValueInvertedIndex, MemoryKeyValueStore},
    MemoryInvertedIndex,
};
use std::{collections::BTreeMap, sync::Arc};
use uqa_core::{
    memory::{BudgetedString, MemoryBudget, MemoryError},
    CancellationToken,
};

mod failures;

fn index(persistent: bool) -> Box<dyn InvertedIndex> {
    let analyzer = uqa_analysis::whitespace_analyzer();
    let mut index: Box<dyn InvertedIndex> = if persistent {
        Box::new(KeyValueInvertedIndex::new(
            Arc::new(MemoryKeyValueStore::new()),
            "docs",
            analyzer,
        ))
    } else {
        Box::new(MemoryInvertedIndex::new(analyzer))
    };
    for (id, value) in [(1, "a a b"), (2, "a")] {
        index
            .add_document(
                id,
                BTreeMap::from([
                    ("long_field".into(), value.into()),
                    ("z".into(), value.into()),
                    ("empty".into(), String::new()),
                ]),
            )
            .unwrap();
    }
    index
}

#[test]
fn query_statistics_keep_term_multiplicity_field_order_and_exact_inventory_owners() {
    for persistent in [false, true] {
        let index = index(persistent);
        let control = StorageReadControl::with_limit(1 << 20);
        let fields = index.field_names_budgeted(&control).unwrap();
        assert_eq!(
            fields.iter().map(|field| &**field).collect::<Vec<_>>(),
            ["empty", "long_field", "z"]
        );
        assert_eq!(
            control.memory().used(),
            fields.capacity() * size_of::<BudgetedString>()
                + fields.iter().map(BudgetedString::capacity).sum::<usize>()
        );
        drop(fields);
        assert_eq!(control.memory().used(), 0);
        assert_eq!(
            query_term_count(&*index, "long_field", "a a b", &control).unwrap(),
            3
        );
        assert_eq!(
            query_document_frequency(&*index, Some("long_field"), "a a b", &control).unwrap(),
            5
        );
        assert_eq!(
            query_document_frequency(&*index, None, "a a b", &control).unwrap(),
            10
        );
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn query_analysis_rejects_before_large_growth_and_preserves_sibling_allowances() {
    let query = "a ".repeat(256 * 1024);
    for persistent in [false, true] {
        let index = index(persistent);
        let parent = MemoryBudget::new(8192);
        let budget = parent.child(1 << 20);
        let sibling = parent.reserve(4096).unwrap();
        let cancellation = CancellationToken::new();
        let control = StorageReadControl::new(&budget, &cancellation);
        for all_fields in [false, true] {
            let allocation = allocation_counter::measure(|| {
                let result = query_document_frequency(
                    &*index,
                    (!all_fields).then_some("long_field"),
                    &query,
                    &control,
                );
                assert!(
                    matches!(
                        result,
                        Err(StorageBackendError::Memory(MemoryError::Limit { .. }))
                    ),
                    "{result:?}"
                );
            });
            assert!(allocation.bytes_total < 64 * 1024, "{allocation:?}");
            assert_eq!(budget.used(), 0);
            assert_eq!(parent.used(), 4096);
        }
        cancellation.cancel();
        assert!(matches!(
            query_term_count(&*index, "long_field", "a", &control),
            Err(StorageBackendError::Cancelled(_))
        ));
        assert_eq!(budget.used(), 0);
        assert_eq!(parent.used(), 4096);
        drop(sibling);
        let recovered = StorageReadControl::new(&budget, &CancellationToken::new());
        assert_eq!(
            query_document_frequency(&*index, Some("long_field"), "a", &recovered).unwrap(),
            2
        );
        assert_eq!(parent.used(), 0);
    }
}

#[test]
fn partial_inventory_rejection_releases_names_and_container() {
    for persistent in [false, true] {
        let index = index(persistent);
        let baseline = StorageReadControl::with_limit(1 << 20);
        let fields = index.field_names_budgeted(&baseline).unwrap();
        let peak = baseline.memory().peak();
        drop(fields);
        for allowance in [0, 1, 32, peak / 2, peak - 1, peak] {
            let control = StorageReadControl::with_limit(allowance);
            let output = index.field_names_budgeted(&control);
            match output {
                Ok(fields) => assert_eq!(fields.len(), 3),
                Err(StorageBackendError::Memory(MemoryError::Limit { .. })) => {
                    assert!(allowance < peak)
                }
                other => panic!("{other:?}"),
            }
            assert_eq!(control.memory().used(), 0);
        }
    }
}
