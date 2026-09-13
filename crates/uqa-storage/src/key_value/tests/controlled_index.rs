//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    clustered_postings::PostingReadCursor, read_control::StorageReadControl, StorageBackendResult,
    TokenTermKey,
};
use uqa_analysis::whitespace_analyzer;

fn fixture(ids: impl IntoIterator<Item = u64>) -> (KeyValueInvertedIndex, Arc<dyn KeyValueStore>) {
    let store = store();
    let mut index = KeyValueInvertedIndex::new(store.clone(), "docs", whitespace_analyzer());
    index
        .try_add_documents(
            ids.into_iter()
                .map(|id| {
                    (
                        id,
                        BTreeMap::from([("body".into(), "alpha alpha beta".into())]),
                    )
                })
                .collect(),
        )
        .unwrap();
    (index, store)
}

#[test]
fn controlled_cursor_matches_owned_blocks_seeks_and_maximum_document() {
    let ids = (0..260).chain([65536, 65538, 8 << 16, u64::MAX]);
    let (index, _) = fixture(ids);
    let term = TokenTermKey::from_text("alpha");
    let control = StorageReadControl::with_limit(1 << 20);
    let other = control.memory().reserve(7).unwrap();
    {
        let mut ordinary = index.posting_cursor_key("body", &term).unwrap();
        let mut cursor = index
            .posting_read_cursor_key_budgeted("body", &term, &control)
            .unwrap();
        assert_eq!(cursor.doc_freq(), ordinary.doc_freq());
        loop {
            assert_eq!(cursor.current(), ordinary.current());
            if ordinary.current().is_none() {
                break;
            }
            cursor.advance().unwrap();
            ordinary.advance().unwrap();
        }
        assert_eq!(cursor.advance_to(0).unwrap(), None);
        assert_eq!(cursor.advance().unwrap(), None);
    }
    assert_eq!(control.memory().used(), 7);
    let mut cursor = index
        .posting_read_cursor_key_budgeted("body", &term, &control)
        .unwrap();
    for target in [
        0,
        127,
        128,
        258,
        259,
        260,
        65537,
        65538,
        (8 << 16) - 1,
        u64::MAX,
    ] {
        let mut ordinary = index.posting_cursor_key("body", &term).unwrap();
        assert_eq!(
            cursor.advance_to(target).unwrap(),
            ordinary.advance_to(target).unwrap()
        );
    }
    assert_eq!(cursor.current().unwrap().doc_id, u64::MAX);
    assert_eq!(cursor.advance().unwrap(), None);
    drop(cursor);
    assert_eq!(control.memory().used(), 7);
    drop(other);
}

#[test]
fn controlled_cursor_resumes_after_failed_loading_without_releasing_live_leases() {
    let (index, _) = fixture([0, 65536, u64::MAX]);
    let term = TokenTermKey::from_text("alpha");
    let control = StorageReadControl::with_limit(1 << 20);
    let other = control.memory().reserve(7).unwrap();
    let mut cursor = index
        .posting_read_cursor_key_budgeted("body", &term, &control)
        .unwrap();
    let initial = cursor.current();
    let live = control.memory().used();
    let hold = control
        .memory()
        .reserve(control.memory().limit() - live)
        .unwrap();
    assert!(matches!(
        cursor.advance(),
        Err(StorageBackendError::Memory(_))
    ));
    assert_eq!(cursor.current(), initial);
    assert_eq!(control.memory().used(), control.memory().limit());
    drop(hold);
    assert_eq!(control.memory().used(), live);
    control.cancellation().cancel();
    assert!(matches!(
        cursor.advance(),
        Err(StorageBackendError::Cancelled(_))
    ));
    assert!(matches!(
        cursor.advance_to(0),
        Err(StorageBackendError::Cancelled(_))
    ));
    assert!(matches!(
        index.get_occurrences_budgeted(0, "body", &term, &control),
        Err(StorageBackendError::Cancelled(_))
    ));
    assert_eq!(cursor.current(), initial);
    control.cancellation().reset();
    let occurrences = index
        .get_occurrences_budgeted(0, "body", &term, &control)
        .unwrap();
    assert_eq!(
        &**occurrences,
        &index.get_occurrences(0, "body", &term).unwrap()
    );
    assert_eq!(
        control.memory().used(),
        live + occurrences.capacity() * size_of::<uqa_core::TokenOccurrence>()
    );
    assert_eq!(cursor.advance().unwrap().unwrap().doc_id, 65536);
    assert_eq!(
        cursor.advance_to(u64::MAX).unwrap().unwrap().doc_id,
        u64::MAX
    );
    drop(cursor);
    assert_eq!(
        control.memory().used(),
        7 + occurrences.capacity() * size_of::<uqa_core::TokenOccurrence>()
    );
    drop(occurrences);
    assert_eq!(control.memory().used(), 7);
    drop(other);
}

fn read(
    index: &KeyValueInvertedIndex,
    term: &TokenTermKey,
    control: &StorageReadControl,
) -> StorageBackendResult<()> {
    let mut cursor = index.posting_read_cursor_key_budgeted("body", term, control)?;
    let stats = index.field_stats_scalar_budgeted("body", control)?;
    assert_eq!(stats.total_docs, 3);
    while let Some(row) = cursor.current() {
        let occurrences = index.get_occurrences_budgeted(row.doc_id, "body", term, control)?;
        assert_eq!(occurrences.len(), 2);
        cursor.advance()?;
    }
    Ok(())
}

#[test]
fn every_quota_boundary_releases_only_the_failed_read_and_preserves_store_contents() {
    let (index, store) = fixture([0, 65536, u64::MAX]);
    let term = TokenTermKey::from_text("alpha");
    let baseline = StorageReadControl::with_limit(1 << 20);
    let other = baseline.memory().reserve(7).unwrap();
    read(&index, &term, &baseline).unwrap();
    let peak = baseline.memory().peak();
    let before = store.scan_prefix(b"").unwrap();
    for limit in 7..=peak {
        let control = StorageReadControl::with_limit(limit);
        let unrelated = control.memory().reserve(7).unwrap();
        let result = read(&index, &term, &control);
        if limit < peak {
            assert!(
                matches!(result, Err(StorageBackendError::Memory(_))),
                "limit={limit}, peak={peak}: {result:?}"
            );
        } else {
            result.unwrap();
        }
        assert_eq!(control.memory().used(), 7, "limit={limit}");
        drop(unrelated);
    }
    assert_eq!(store.scan_prefix(b"").unwrap(), before);
    drop(other);
}

#[test]
fn controlled_reads_preserve_format_metadata_and_cluster_corruption_errors() {
    use crate::key_value::occurrence_keys as keys;
    let (index, store) = fixture([0, 65536]);
    let term = TokenTermKey::from_text("alpha");
    let control = StorageReadControl::with_limit(1 << 20);
    let marker = keys::kind_prefix("docs", keys::FORMAT).unwrap();
    let score_key = keys::cluster_key("docs", keys::SCORE, "body", &term, 1).unwrap();
    let positions = keys::cluster_key("docs", keys::POSITIONS, "body", &term, 0).unwrap();
    let metadata = keys::metadata_key("docs", "body", 0).unwrap();
    for key in [marker, score_key, positions, metadata] {
        let old = store.get(&key).unwrap().unwrap();
        store.put(&key, b"invalid").unwrap();
        if key == keys::cluster_key("docs", keys::SCORE, "body", &term, 1).unwrap() {
            assert!(index
                .posting_read_cursor_key_budgeted("body", &term, &control)
                .is_err());
        } else {
            assert!(index
                .get_occurrences_budgeted(0, "body", &term, &control)
                .is_err());
        }
        assert_eq!(control.memory().used(), 0);
        store.put(&key, &old).unwrap();
    }
    let mut calls = 0;
    assert!(matches!(
        index.visit_score_clusters("body", &term, None, usize::MAX, &control, &mut |_| {
            calls += 1;
            control.cancellation().cancel();
            Ok(())
        }),
        Err(StorageBackendError::Cancelled(_))
    ));
    assert_eq!(calls, 1);
    assert_eq!(control.memory().used(), 0);
    control.cancellation().reset();
    index
        .posting_read_cursor_key_budgeted("body", &term, &control)
        .unwrap();
}

#[test]
fn retained_cursor_memory_does_not_grow_with_the_number_of_clusters() {
    let term = TokenTermKey::from_text("alpha");
    let mut peaks = Vec::new();
    for count in [2, 128] {
        let (index, _) = fixture((0..count).map(|id| id << 16));
        let control = StorageReadControl::with_limit(1 << 20);
        {
            let mut cursor = index
                .posting_read_cursor_key_budgeted("body", &term, &control)
                .unwrap();
            assert_eq!(cursor.doc_freq(), count);
            while cursor.current().is_some() {
                cursor.advance().unwrap();
            }
        }
        assert_eq!(control.memory().used(), 0);
        peaks.push(control.memory().peak());
    }
    assert_eq!(peaks[0], peaks[1]);
}
