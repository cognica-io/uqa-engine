//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Score-ordered views over document-id-ordered posting storage.

use std::cmp::Ordering;
use std::sync::OnceLock;

use crate::{PostingEntry, PostingList};

/// A score-ordered, non-owning view of a [`PostingList`].
///
/// Posting storage remains sorted by document id. The separate order by
/// descending payload score is built lazily, with ascending document id as the
/// deterministic tie-breaker. This lets support-preserving selections such as
/// `select_top_k(len)` avoid ranking work entirely.
#[derive(Debug)]
pub struct RankedView<'a> {
    source: &'a PostingList,
    entries: OnceLock<Vec<&'a PostingEntry>>,
}

impl<'a> RankedView<'a> {
    pub(crate) fn new(posting_list: &'a PostingList) -> Self {
        Self {
            source: posting_list,
            entries: OnceLock::new(),
        }
    }

    fn compare_rank(left: &PostingEntry, right: &PostingEntry) -> Ordering {
        right
            .payload
            .score
            .total_cmp(&left.payload.score)
            .then_with(|| left.doc_id.cmp(&right.doc_id))
    }

    fn rank_entries(posting_list: &'a PostingList) -> Vec<&'a PostingEntry> {
        let mut entries: Vec<&PostingEntry> = posting_list.entries().iter().collect();
        entries.sort_by(|left, right| Self::compare_rank(left, right));
        entries
    }

    /// Borrow every entry in rank order.
    pub fn entries(&self) -> &[&'a PostingEntry] {
        self.entries.get_or_init(|| Self::rank_entries(self.source))
    }

    /// Borrow at most the first `k` entries in rank order.
    pub fn top_k(&self, k: usize) -> &[&'a PostingEntry] {
        if k == 0 {
            return &[];
        }
        let entries = self.entries();
        &entries[..k.min(entries.len())]
    }

    /// Materialize the top `k` selection as document-id-ordered posting
    /// storage.
    ///
    /// Rank order intentionally does not leak into [`PostingList`]'s physical
    /// ordering invariant. If the view has not already been ranked, this uses
    /// linear-time selection rather than sorting entries that will immediately
    /// be reordered by document id. Use [`Self::top_k`] when rank order itself
    /// is needed.
    pub fn select_top_k(self, k: usize) -> PostingList {
        if k == 0 {
            return PostingList::new();
        }
        if k >= self.source.len() {
            return self.source.clone();
        }

        let source = self.source;
        let selected = if let Some(ranked) = self.entries.into_inner() {
            ranked
        } else {
            let mut candidates: Vec<&PostingEntry> = source.entries().iter().collect();
            candidates.select_nth_unstable_by(k, |left, right| Self::compare_rank(left, right));
            candidates
        };
        let mut entries: Vec<PostingEntry> = selected.into_iter().take(k).cloned().collect();
        entries.sort_by_key(|entry| entry.doc_id);
        PostingList::from_sorted_unchecked(entries)
    }

    /// Number of ranked entries.
    pub fn len(&self) -> usize {
        self.source.len()
    }

    /// Whether the view contains no entries.
    pub fn is_empty(&self) -> bool {
        self.source.is_empty()
    }

    /// Iterate in descending score order.
    pub fn iter(
        &self,
    ) -> impl ExactSizeIterator<Item = &'a PostingEntry> + DoubleEndedIterator + '_ {
        self.entries().iter().copied()
    }
}

#[cfg(test)]
mod tests {
    use crate::{Payload, PostingEntry, PostingList};

    #[test]
    fn ranking_is_separate_from_posting_storage_order() {
        let posting = PostingList::from_unsorted(vec![
            PostingEntry::new(1, Payload::with_score(0.2)),
            PostingEntry::new(2, Payload::with_score(0.9)),
            PostingEntry::new(3, Payload::with_score(0.9)),
        ]);

        let ranked = posting.ranked();
        let ranked_ids: Vec<_> = ranked.iter().map(|entry| entry.doc_id).collect();
        assert_eq!(ranked_ids, vec![2, 3, 1]);

        let selected = ranked.select_top_k(2);
        assert_eq!(selected.doc_ids().collect::<Vec<_>>(), vec![2, 3]);
    }

    #[test]
    fn selecting_the_entire_source_preserves_every_payload() {
        let posting = PostingList::from_unsorted(vec![
            PostingEntry::new(1, Payload::with_score(0.2)),
            PostingEntry::new(2, Payload::with_score(0.9)),
            PostingEntry::new(3, Payload::with_score(0.5)),
        ]);

        assert_eq!(posting.ranked().select_top_k(posting.len()), posting);
    }

    #[test]
    fn selecting_zero_entries_is_empty() {
        let posting =
            PostingList::from_unsorted(vec![PostingEntry::new(1, Payload::with_score(0.2))]);

        assert!(posting.ranked().top_k(0).is_empty());
        assert!(posting.ranked().select_top_k(0).is_empty());
    }

    #[test]
    fn materialized_selection_matches_the_full_rank_order() {
        let posting = PostingList::from_unsorted(vec![
            PostingEntry::new(1, Payload::with_score(f64::NAN)),
            PostingEntry::new(2, Payload::with_score(0.9)),
            PostingEntry::new(3, Payload::with_score(0.9)),
            PostingEntry::new(4, Payload::with_score(f64::INFINITY)),
            PostingEntry::new(5, Payload::with_score(f64::NEG_INFINITY)),
        ]);
        let ranked_entries = posting.ranked().entries().to_vec();

        for k in 0..=posting.len() + 1 {
            let mut expected: Vec<_> = ranked_entries
                .iter()
                .take(k)
                .map(|entry| (entry.doc_id, entry.payload.score.to_bits()))
                .collect();
            expected.sort_unstable_by_key(|(doc_id, _)| *doc_id);

            let actual: Vec<_> = posting
                .ranked()
                .select_top_k(k)
                .entries()
                .iter()
                .map(|entry| (entry.doc_id, entry.payload.score.to_bits()))
                .collect();
            assert_eq!(actual, expected);
        }
    }
}
