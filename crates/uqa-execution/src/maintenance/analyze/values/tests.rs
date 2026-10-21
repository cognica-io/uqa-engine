//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_storage::document_store::MemoryDocumentStore;

#[test]
fn analyze_capture_bounds_complete_population_and_keeps_column_order() {
    let mut documents = MemoryDocumentStore::new();
    for id in 0..32_768 {
        documents
            .put(
                id,
                [
                    ("id".into(), Value::Int(i64::try_from(32_767 - id).unwrap())),
                    ("body".into(), Value::Str("alpha beta".into())),
                ]
                .into(),
            )
            .unwrap();
    }
    let snapshot = documents.snapshot().unwrap();
    let allocated = allocation_counter::measure(|| {
        let observations = collect_values(
            [Ok(Arc::clone(&snapshot))],
            &["id".into(), "body".into()],
            64 * 1024,
            &CancellationToken::new(),
            |_| true,
        )
        .unwrap();
        assert_eq!(observations.row_count, 32_768);
        assert!(observations.counts.iter().all(|counts| {
            counts.nulls == 0 && counts.omitted == 0 && counts.comparable == 32_768
        }));
        let mut values = observations.values;
        for id in 0..32_768 {
            assert_eq!(values.next().unwrap().unwrap(), (0, Value::Int(id)));
        }
        for _ in 0..32_768 {
            assert_eq!(
                values.next().unwrap().unwrap(),
                (1, Value::Str("alpha beta".into()))
            );
        }
        assert!(values.next().is_none());
    });
    assert!(allocated.bytes_max < 2 * 1024 * 1024, "{allocated:?}");
}

#[test]
fn analyze_capture_preserves_hierarchy_null_and_omission_counts() {
    let mut first = MemoryDocumentStore::new();
    first
        .put(u64::MAX, [("v".into(), Value::Int(7))].into())
        .unwrap();
    let mut second = MemoryDocumentStore::new();
    second
        .put(0, [("v".into(), Value::Str("omitted".into()))].into())
        .unwrap();
    second.put(2, [("v".into(), Value::Null)].into()).unwrap();
    let first_snapshot = first.snapshot().unwrap();
    first
        .put(u64::MAX, [("v".into(), Value::Int(9))].into())
        .unwrap();
    let observed = collect_values(
        [Ok(first_snapshot), second.snapshot()],
        &["v".into()],
        0,
        &CancellationToken::new(),
        |value| !matches!(value, Value::Str(_)),
    )
    .unwrap();
    assert_eq!(observed.row_count, 3);
    assert_eq!(observed.counts[0].nulls, 1);
    assert_eq!(observed.counts[0].omitted, 1);
    assert_eq!(observed.counts[0].comparable, 1);
    let values = observed.values.collect::<Result<Vec<_>, _>>().unwrap();
    assert_eq!(values, vec![(0, Value::Int(7))]);
}

#[test]
fn analyze_capture_checks_cancellation_before_snapshot_and_distribution_reads() {
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    let source = std::iter::from_fn(|| -> Option<StorageBackendResult<Arc<dyn DocumentStore>>> {
        panic!("cancelled collection must not acquire a snapshot")
    });
    assert_eq!(
        collect_values(source, &[], 0, &cancellation, |_| true)
            .err()
            .unwrap()
            .sqlstate(),
        Some("57014"),
    );
    cancellation.reset();
    let mut documents = MemoryDocumentStore::new();
    documents
        .put(1, [("v".into(), Value::Int(1))].into())
        .unwrap();
    let mut observed = collect_values(
        [documents.snapshot()],
        &["v".into()],
        0,
        &cancellation,
        |_| true,
    )
    .unwrap();
    cancellation.cancel();
    assert_eq!(
        observed.values.next().unwrap().unwrap_err().sqlstate(),
        Some("57014")
    );
}
