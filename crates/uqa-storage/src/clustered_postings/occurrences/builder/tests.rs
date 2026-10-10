//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::clustered_postings::decode_occurrence_cluster;

#[test]
fn finished_clusters_belong_to_the_output_allowance() {
    let input = StorageReadControl::with_limit(1 << 20);
    let output = StorageReadControl::with_limit(1 << 20);
    let occurrence = TokenOccurrence {
        position: 0,
        position_length: 1,
        offsets: None,
    };
    let mut builder = OccurrenceClusterBuilder::new(&input).unwrap();
    for id in 1..=257 {
        builder
            .push(id, 1, std::slice::from_ref(&occurrence), &input)
            .unwrap();
    }
    let (scores, positions) = builder.finish(&output).unwrap();
    assert_eq!(input.memory().used(), 0);
    assert!(output.memory().used() >= scores.len() + positions.len());
    assert_eq!(
        decode_occurrence_cluster(0, &scores, &positions)
            .unwrap()
            .len(),
        257
    );
    drop((scores, positions));
    assert_eq!(output.memory().used(), 0);
}

#[test]
fn incremental_cluster_encoding_retains_only_one_decoded_score_block() {
    for count in [128, 512, 20_000] {
        let control = StorageReadControl::with_limit(1 << 20);
        let retained = control.memory().reserve(256 << 10).unwrap();
        let occurrence = TokenOccurrence {
            position: 3,
            position_length: 2,
            offsets: None,
        };
        let mut builder = OccurrenceClusterBuilder::new(&control).unwrap();
        for id in 1..=count {
            builder
                .push(id, 7, std::slice::from_ref(&occurrence), &control)
                .unwrap();
        }
        let (scores, positions) = builder.finish(&control).unwrap();
        let decoded = decode_occurrence_cluster(0, &scores, &positions).unwrap();
        assert_eq!(decoded.len(), count as usize);
        for (offset, posting) in decoded.iter().enumerate() {
            assert_eq!(posting.doc_id, offset as u64 + 1);
            assert_eq!(posting.doc_length, 7);
            assert_eq!(posting.occurrences, [occurrence]);
        }
        assert!(control.memory().peak() <= 1 << 20);
        drop((scores, positions, retained));
        assert_eq!(control.memory().used(), 0);
    }
}
