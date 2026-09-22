//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{nearest_centroids, scored_posting_list};
use crate::{read_control::StorageReadControl, StorageBackendError};

#[test]
fn centroid_probes_preserve_ordinal_ties_and_retain_only_the_result_buffer() {
    let centroids = [
        vec![1.0, 0.0],
        vec![0.0, 1.0],
        vec![1.0, 0.0],
        vec![-1.0, 0.0],
    ];
    let control = StorageReadControl::with_limit(4096);
    for (query, count, expected) in [
        ([2.0, 0.0], 0, vec![0]),
        ([2.0, 0.0], 2, vec![0, 2]),
        ([0.0, 0.0], 10, vec![0, 1, 2, 3]),
    ] {
        let probes = nearest_centroids(&query, &centroids, count, Some(&control)).unwrap();
        assert_eq!(&*probes, expected);
        assert_eq!(
            &*nearest_centroids(&query, &centroids, count, None).unwrap(),
            expected
        );
        assert!(control.memory().used() >= expected.len() * size_of::<usize>());
        let held = control
            .memory()
            .reserve(control.memory().limit() - control.memory().used())
            .unwrap();
        assert!(matches!(
            nearest_centroids(&query, &centroids, count, Some(&control)),
            Err(StorageBackendError::Memory(_))
        ));
        assert_eq!(&*probes, expected);
        drop((held, probes));
        assert_eq!(control.memory().used(), 0);
    }
    control.cancellation().cancel();
    assert!(matches!(
        nearest_centroids(&[1.0, 0.0], &centroids, 1, Some(&control)),
        Err(StorageBackendError::Cancelled(_))
    ));
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn provider_scores_preserve_tensor_maxima_ties_and_posting_order() {
    let control = StorageReadControl::with_limit(1 << 20);
    let entries = [
        (9, [0.0, 1.0]),
        (3, [-1.0, 0.0]),
        (9, [1.0, 0.0]),
        (1, [1.0, 0.0]),
    ];
    for (k, expected) in [
        (0, vec![]),
        (1, vec![(1, 1.0)]),
        (2, vec![(1, 1.0), (9, 1.0)]),
        (8, vec![(1, 1.0), (3, -1.0), (9, 1.0)]),
    ] {
        let read = || {
            entries
                .iter()
                .map(|(doc, vector)| (*doc, vector.as_slice()))
        };
        let result = scored_posting_list(&[1.0, 0.0], read(), k, Some(&control)).unwrap();
        assert_eq!(
            result,
            scored_posting_list(&[1.0, 0.0], read(), k, None).unwrap()
        );
        assert_eq!(
            result
                .iter()
                .map(|entry| (entry.doc_id, entry.payload.score))
                .collect::<Vec<_>>(),
            expected
        );
        // PostingList keeps its established caller-owned output boundary.
        assert_eq!(control.memory().used(), 0);
    }
    let large = [
        (1, [1.0e30, 0.0]),
        (1, [1.0, 0.0]),
        (2, [1.0, 0.0]),
        (2, [1.0e30, 0.0]),
    ];
    let result = scored_posting_list(
        &[1.0e30, 0.0],
        large.iter().map(|(doc, vector)| (*doc, vector.as_slice())),
        8,
        Some(&control),
    )
    .unwrap();
    assert_eq!(
        result
            .iter()
            .map(|entry| (entry.doc_id, entry.payload.score))
            .collect::<Vec<_>>(),
        [(1, 0.0), (2, 0.0)]
    );
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn candidate_scoring_unwinds_quota_and_midstream_cancellation_without_replaying_input() {
    let control = StorageReadControl::with_limit(4096);
    let vector = [1.0, 0.0];
    let mut calls = 0;
    let input = (1..=3).map(|doc| {
        calls += 1;
        if doc == 2 {
            control.cancellation().cancel();
        }
        (doc, vector.as_slice())
    });
    assert!(matches!(
        scored_posting_list(&vector, input, 2, Some(&control)),
        Err(StorageBackendError::Cancelled(_))
    ));
    assert_eq!(calls, 2);
    assert_eq!(control.memory().used(), 0);
    assert!(control.memory().peak() > 0);
    control.cancellation().reset();
    let held = control.memory().reserve(control.memory().limit()).unwrap();
    assert!(matches!(
        scored_posting_list(&vector, [(1, vector.as_slice())], 1, Some(&control)),
        Err(StorageBackendError::Memory(_))
    ));
    assert_eq!(control.memory().used(), control.memory().limit());
    drop(held);
    assert_eq!(
        scored_posting_list(&vector, [(1, vector.as_slice())], 1, Some(&control))
            .unwrap()
            .doc_ids()
            .collect::<Vec<_>>(),
        [1]
    );
    assert_eq!(control.memory().used(), 0);
}
