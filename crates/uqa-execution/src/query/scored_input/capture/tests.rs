//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn scored_capture_replay_shares_input_and_releases_the_final_owner() {
    let cancellation = CancellationToken::new();
    let mut capture = ScoredEntryCapture::new(0, cancellation).unwrap();
    for doc_id in [0, 7, u64::MAX] {
        capture
            .push(ScoredEntry {
                doc_id,
                score: -0.0,
            })
            .unwrap();
    }
    let path = capture.rows.spill_path().unwrap().to_path_buf();
    let mut original = capture.finish().unwrap();
    let mut replay = original.replay().unwrap();
    assert_eq!(original.next().unwrap().unwrap().doc_id, 0);
    assert!(original.replay().is_err());
    drop(original);
    assert!(path.exists());
    for id in [0, 7, u64::MAX] {
        let entry = replay.next().unwrap().unwrap();
        assert_eq!(entry.doc_id, id);
        assert_eq!(entry.score.to_bits(), (-0.0_f64).to_bits());
    }
    assert!(replay.next().is_none());
    assert!(!path.exists());
}

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

#[test]
fn score_only_merge_releases_both_inputs_after_cancellation_or_empty_support() {
    for cancelled in [false, true] {
        let cancellation = CancellationToken::new();
        let mut inputs = Vec::new();
        let mut paths = Vec::new();
        for id in [1, 2] {
            let mut capture = ScoredEntryCapture::new(0, cancellation.clone()).unwrap();
            capture
                .push(ScoredEntry {
                    doc_id: id,
                    score: 0.5,
                })
                .unwrap();
            paths.push(capture.rows.spill_path().unwrap().to_path_buf());
            inputs.push(capture.finish().unwrap());
        }
        let right = inputs.pop().unwrap();
        let left = inputs.pop().unwrap();
        if cancelled {
            cancellation.cancel();
        }
        let result = left.merge_text_scores(right, true, 0, cancellation);
        if cancelled {
            assert_eq!(result.err().unwrap().sqlstate(), Some("57014"));
        } else {
            assert_eq!(result.unwrap().count(), 0);
        }
        assert!(paths.iter().all(|path| !path.exists()));
    }
}

#[test]
fn field_score_merge_preserves_native_maximum_and_unmatched_bits() {
    let fields = [
        [(0, -0.0), (1, 5.0), (u64::MAX, f64::NAN)],
        [(0, 0.0), (2, 0.25), (u64::MAX, 7.0)],
        [(1, 2.0), (3, -0.0), (u64::MAX, 4.0)],
    ];
    let mut expected = std::collections::BTreeMap::<u64, f64>::new();
    let mut actual: Option<CapturedScoredEntries> = None;
    for field in fields {
        let mut capture = ScoredEntryCapture::new(0, CancellationToken::new()).unwrap();
        for (doc_id, score) in field {
            expected
                .entry(doc_id)
                .and_modify(|previous| *previous = previous.max(score))
                .or_insert(score);
            capture.push(ScoredEntry { doc_id, score }).unwrap();
        }
        let next = capture.finish().unwrap();
        actual = Some(match actual {
            Some(previous) => previous
                .merge_field_scores(next, 0, CancellationToken::new())
                .unwrap(),
            None => next,
        });
    }
    let actual = actual
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            (entry.doc_id, entry.score.to_bits())
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    assert_eq!(
        actual,
        expected
            .into_iter()
            .map(|(id, score)| (id, score.to_bits()))
            .collect()
    );
}
