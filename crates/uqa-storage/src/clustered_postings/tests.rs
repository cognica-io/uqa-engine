//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

mod allocation;

fn posting(doc_id: DocId, positions: &[u32], doc_length: u64) -> ClusterPosting {
    ClusterPosting {
        doc_id,
        term_freq: positions.len() as u64,
        doc_length,
        positions: positions.to_vec(),
    }
}

#[test]
fn clustered_round_trip_separates_scores_and_positions() {
    let entries = vec![
        posting(2, &[1, 4], 8),
        posting(9, &[0], 3),
        posting(65_535, &[2, 7, 11], 20),
    ];
    let (scores, positions) = encode_cluster(&entries).unwrap();
    assert_eq!(score_count(&scores).unwrap(), 3);
    assert_eq!(decode_all_scores(0, &scores).unwrap()[0].term_freq, 2);
    assert_eq!(decode_cluster(0, &scores, &positions).unwrap(), entries);
}

#[test]
fn lazy_cursor_decodes_across_blocks_and_clusters() {
    let first = (0..260_u64)
        .map(|doc_id| posting(doc_id * 2, &[0], 4))
        .collect::<Vec<_>>();
    let second = vec![posting(POSTING_CLUSTER_DOCS + 7, &[1, 3], 9)];
    let (first_scores, _) = encode_cluster(&first).unwrap();
    let (second_scores, _) = encode_cluster(&second).unwrap();
    let mut cursor = ClusteredPostingCursor::new(vec![
        EncodedScoreCluster {
            cluster_id: 0,
            bytes: first_scores,
        },
        EncodedScoreCluster {
            cluster_id: 1,
            bytes: second_scores,
        },
    ])
    .unwrap();
    assert_eq!(cursor.doc_freq(), 261);
    assert_eq!(cursor.current().unwrap().doc_id, 0);
    assert_eq!(cursor.advance_to(400).unwrap().unwrap().doc_id, 400);
    assert_eq!(cursor.ordinal(), 200);
    assert_eq!(
        cursor
            .advance_to(POSTING_CLUSTER_DOCS + 1)
            .unwrap()
            .unwrap(),
        PostingScore {
            doc_id: POSTING_CLUSTER_DOCS + 7,
            term_freq: 2,
            doc_length: 9,
        }
    );
    assert!(cursor.advance().unwrap().is_none());
}

#[test]
fn malformed_cluster_is_rejected_before_iteration() {
    let entries = vec![posting(1, &[0], 1)];
    let (mut scores, positions) = encode_cluster(&entries).unwrap();
    scores[4] = 99;
    assert!(decode_cluster(0, &scores, &positions).is_err());
    assert!(ClusteredPostingCursor::new(vec![EncodedScoreCluster {
        cluster_id: 0,
        bytes: scores,
    }])
    .is_err());
}

#[test]
fn cursor_rejects_overlapping_block_ranges_before_iteration() {
    let entries = (0..130_u64)
        .map(|doc_id| posting(doc_id, &[0], 1))
        .collect::<Vec<_>>();
    let (mut scores, _) = encode_cluster(&entries).unwrap();
    let second_directory = HEADER_LEN + SCORE_DIRECTORY_ENTRY_LEN;
    let second_docs_start = read_u32(&scores, second_directory + 4).unwrap() as usize;
    scores[second_docs_start] = 0;
    assert!(ClusteredPostingCursor::new(vec![EncodedScoreCluster {
        cluster_id: 0,
        bytes: scores,
    }])
    .is_err());
}

#[test]
fn empty_cluster_has_no_persisted_representation() {
    assert!(encode_cluster(&[]).is_err());
}

#[test]
fn document_terms_round_trip() {
    let terms = vec!["alpha".to_string(), "rust".to_string(), "검색".to_string()];
    assert_eq!(decode_terms(&encode_terms(&terms).unwrap()).unwrap(), terms);
    assert!(encode_terms(&["same".into(), "same".into()]).is_err());
    let mut oversized_count = encode_terms(&[]).unwrap();
    oversized_count[8..12].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(decode_terms(&oversized_count).is_err());
}
