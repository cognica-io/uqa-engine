// Unified Query Algebra
// Copyright (c) 2023-2026 Cognica, Inc.
//
// libfuzzer target: posting list construction is robust against
// arbitrary `(doc_id, score)` payloads. We feed `arbitrary` data into
// `PostingList::from_unsorted` and assert the resulting list is sorted
// with no duplicate doc ids — the two invariants the constructor is
// supposed to enforce.
// Run with: cargo +nightly fuzz run posting_list_round_trip

#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use uqa_core::{Payload, PostingEntry, PostingList};

#[derive(Arbitrary, Debug)]
struct EntrySeed {
    doc_id: u64,
    score: f64,
}

fuzz_target!(|seeds: Vec<EntrySeed>| {
    let entries: Vec<PostingEntry> = seeds
        .into_iter()
        .map(|s| PostingEntry {
            doc_id: s.doc_id,
            payload: Payload {
                positions: Vec::new(),
                score: if s.score.is_finite() { s.score } else { 0.0 },
                fields: Default::default(),
            },
        })
        .collect();
    let pl = PostingList::from_unsorted(entries);
    let ids: Vec<u64> = pl.doc_ids().collect();
    assert!(ids.windows(2).all(|w| w[0] < w[1]),
        "from_unsorted produced unsorted or duplicate doc_ids: {ids:?}");
});
