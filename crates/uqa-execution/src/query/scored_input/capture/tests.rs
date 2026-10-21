//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn complete_scored_capture_spills_before_retaining_the_population() {
    let cancellation = CancellationToken::new();
    let allocated = allocation_counter::measure(|| {
        let mut capture = ScoredEntryCapture::new(64 * 1024, cancellation.clone()).unwrap();
        for ordinal in 0..65_536 {
            capture
                .push(ScoredEntry {
                    doc_id: u64::MAX - ordinal,
                    score: f64::from_bits(ordinal),
                })
                .unwrap();
        }
        assert!(capture.rows.has_spilled());
        let mut entries = capture.finish().unwrap();
        for ordinal in 0..65_536 {
            let entry = entries.next().unwrap().unwrap();
            assert_eq!(entry.doc_id, u64::MAX - ordinal);
            assert_eq!(entry.score.to_bits(), ordinal);
        }
        assert!(entries.next().is_none());
    });
    // A complete score vector alone needs 1MiB, excluding posting payloads.
    assert!(allocated.bytes_max < 512 * 1024, "{allocated:?}");
}

#[test]
fn scored_capture_preserves_duplicate_identity_order_and_score_bits() {
    for allowance in [0, 64 * 1024] {
        let mut capture = ScoredEntryCapture::new(allowance, CancellationToken::new()).unwrap();
        let scores = [-0.0, f64::from_bits(0x7ff8_0000_0000_0042), 0.0, 1.0];
        for (position, score) in scores.iter().enumerate() {
            capture
                .push(ScoredEntry {
                    doc_id: u64::from(position > 1),
                    score: *score,
                })
                .unwrap();
        }
        let result = capture
            .finish()
            .unwrap()
            .map(Result::unwrap)
            .collect::<Vec<_>>();
        for (position, entry) in result.iter().enumerate() {
            assert_eq!(entry.doc_id, u64::from(position > 1));
            assert_eq!(entry.score.to_bits(), scores[position].to_bits());
        }
    }
}

#[test]
fn scored_capture_cancelled_replay_releases_unread_spill() {
    let cancellation = CancellationToken::new();
    let mut capture = ScoredEntryCapture::new(0, cancellation.clone()).unwrap();
    capture
        .push(ScoredEntry {
            doc_id: 1,
            score: 0.5,
        })
        .unwrap();
    let path = capture.rows.spill_path().unwrap().to_path_buf();
    let mut rows = capture.finish().unwrap();
    assert!(path.exists());
    cancellation.cancel();
    assert_eq!(rows.next().unwrap().unwrap_err().sqlstate(), Some("57014"));
    assert!(rows.next().is_none());
    assert!(!path.exists());
}

#[test]
fn scored_capture_drop_removes_unread_spill() {
    let mut capture = ScoredEntryCapture::new(0, CancellationToken::new()).unwrap();
    capture
        .push(ScoredEntry {
            doc_id: 1,
            score: 0.5,
        })
        .unwrap();
    let path = capture.rows.spill_path().unwrap().to_path_buf();
    let rows = capture.finish().unwrap();
    assert!(path.exists());
    drop(rows);
    assert!(!path.exists());
}
