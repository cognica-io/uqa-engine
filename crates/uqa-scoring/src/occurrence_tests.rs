//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A provider contract whose compatibility positions intentionally contain fewer values than its term frequency.

use std::collections::BTreeMap;
use std::sync::Arc;

use uqa_analysis::{Analyzer, AnalyzerLimits, AnalyzerResources, TokenLengthPolicy};
use uqa_core::{DocId, FieldName, IndexStats, Payload, PostingEntry, PostingList};
use uqa_storage::clustered_postings::{
    encode_occurrence_cluster, ClusteredPostingCursor, EncodedScoreCluster, OccurrencePosting,
};
use uqa_storage::inverted_index::analyze_index_field;
use uqa_storage::{
    BlockMaxIndex, InvertedIndex, StorageBackendError, StorageBackendResult, TokenTermKey,
};

use crate::{
    BM25Params, BM25Scorer, BlockMaxWANDScorer, CursorBlockMaxWANDScorer, CursorWANDQuery,
    CursorWANDScorer, WANDQuery, WANDScorer,
};

#[derive(Clone)]
pub(crate) struct OccurrenceIndex {
    analyzer: Analyzer,
    pub(crate) entries: Vec<OccurrencePosting>,
}

impl OccurrenceIndex {
    pub(crate) fn new() -> Self {
        let analyzer: Analyzer = serde_json::from_str(r#"{"tokenizer":{"type":"whitespace"},"token_filters":[{"type":"synonym","synonyms":{"a":["a","a"]}}]}"#).unwrap();
        let compiled = AnalyzerResources::new(AnalyzerLimits::default())
            .compile_with_length_policy(&analyzer, TokenLengthPolicy::DiscountOverlaps)
            .unwrap();
        let entries = [(1, "a"), (2, "a a")]
            .into_iter()
            .map(|(doc_id, text)| {
                let mut field = analyze_index_field(&compiled, text).unwrap();
                OccurrencePosting {
                    doc_id,
                    doc_length: field.length,
                    occurrences: field.terms.remove(&TokenTermKey::from_text("a")).unwrap(),
                }
            })
            .collect();
        Self { analyzer, entries }
    }
}

fn read_only() -> StorageBackendResult<()> {
    Err(StorageBackendError::Other(
        "read-only occurrence test index".into(),
    ))
}

impl InvertedIndex for OccurrenceIndex {
    fn analyzer(&self) -> &Analyzer {
        &self.analyzer
    }
    fn add_document(
        &mut self,
        _: DocId,
        _: BTreeMap<FieldName, String>,
    ) -> StorageBackendResult<()> {
        read_only()
    }
    fn remove_document(&mut self, _: DocId) -> StorageBackendResult<()> {
        read_only()
    }
    fn clear(&mut self) -> StorageBackendResult<()> {
        read_only()
    }
    fn get_posting_list(&self, field: &str, term: &str) -> StorageBackendResult<PostingList> {
        if field != "body" || term != "a" {
            return Ok(PostingList::new());
        }
        Ok(PostingList::from_sorted_unchecked(
            self.entries
                .iter()
                .map(|entry| {
                    PostingEntry::new(
                        entry.doc_id,
                        Payload {
                            positions: entry.positions(),
                            ..Payload::default()
                        },
                    )
                })
                .collect(),
        ))
    }
    fn doc_freq(&self, field: &str, term: &str) -> StorageBackendResult<u64> {
        Ok(if field == "body" && term == "a" {
            self.entries.len() as u64
        } else {
            0
        })
    }
    fn get_doc_length(&self, doc_id: DocId, field: &str) -> StorageBackendResult<u64> {
        Ok(self
            .entries
            .iter()
            .find(|entry| field == "body" && entry.doc_id == doc_id)
            .map_or(0, |entry| entry.doc_length))
    }
    fn get_term_freq(&self, doc_id: DocId, field: &str, term: &str) -> StorageBackendResult<u64> {
        Ok(self
            .entries
            .iter()
            .find(|entry| field == "body" && term == "a" && entry.doc_id == doc_id)
            .map_or(0, |entry| entry.score().term_freq))
    }
    fn doc_count(&self) -> StorageBackendResult<u64> {
        Ok(self.entries.len() as u64)
    }
    fn total_field_length(&self, field: &str) -> StorageBackendResult<u64> {
        Ok(if field == "body" {
            self.entries.iter().map(|entry| entry.doc_length).sum()
        } else {
            0
        })
    }
    fn field_doc_count(&self, field: &str) -> StorageBackendResult<u64> {
        Ok(if field == "body" {
            self.entries.len() as u64
        } else {
            0
        })
    }
    fn stats(&self) -> StorageBackendResult<IndexStats> {
        let mut stats = IndexStats::new(self.entries.len() as u64).with_doc_freq(
            "body",
            "a",
            self.entries.len() as u64,
        );
        stats.avg_doc_length = self.total_field_length("body")? as f64 / self.entries.len() as f64;
        Ok(stats)
    }
    fn snapshot(&self) -> StorageBackendResult<Arc<dyn InvertedIndex>> {
        Ok(Arc::new(self.clone()))
    }
}

#[test]
fn projected_positions_do_not_replace_occurrence_frequency_or_normalization_length() {
    let index = OccurrenceIndex::new();
    assert_eq!(
        index.get_posting_list("body", "a").unwrap().entries()[0]
            .payload
            .positions,
        [0]
    );
    let cursor = index.posting_cursor("body", "a").unwrap();
    assert_eq!(cursor.current().unwrap().term_freq, 3);
    assert_eq!(cursor.current().unwrap().doc_length, 1);
    let mut frequencies = Vec::new();
    index
        .for_each_term_freq("body", "a", &mut |doc, frequency| {
            frequencies.push((doc, frequency));
        })
        .unwrap();
    assert_eq!(frequencies, [(1, 3), (2, 6)]);
}

#[test]
fn all_wand_paths_match_exact_graph_frequency_and_discounted_length_scores() {
    let reference = OccurrenceIndex::new();
    verify_wand_scores(&reference, || {
        let (bytes, _) = encode_occurrence_cluster(&reference.entries).unwrap();
        Box::new(
            ClusteredPostingCursor::new(vec![EncodedScoreCluster {
                cluster_id: 0,
                bytes,
            }])
            .unwrap(),
        )
    });
}

#[test]
fn memory_graph_postings_produce_exact_scores_in_every_wand_path() {
    let reference = OccurrenceIndex::new();
    let revision = AnalyzerResources::default()
        .compile_with_length_policy(&reference.analyzer, TokenLengthPolicy::DiscountOverlaps)
        .unwrap();
    let mut index = uqa_storage::MemoryInvertedIndex::new(reference.analyzer);
    index
        .set_field_analyzer_revision("body", revision, uqa_storage::AnalyzerPhase::Both)
        .unwrap();
    index
        .try_add_documents(
            [(1, "a"), (2, "a a")]
                .into_iter()
                .map(|(doc, text)| (doc, BTreeMap::from([("body".into(), text.into())])))
                .collect(),
        )
        .unwrap();
    verify_wand_scores(&index, || index.posting_cursor("body", "a").unwrap());
}

fn verify_wand_scores(
    index: &dyn InvertedIndex,
    cursor: impl Fn() -> Box<dyn uqa_storage::PostingCursor>,
) {
    let scorer = Arc::new(BM25Scorer::new(
        BM25Params::default(),
        Arc::new(index.stats().unwrap()),
    ));
    let expected: BTreeMap<_, _> = [(1, 3, 1), (2, 6, 2)]
        .into_iter()
        .map(|(doc, frequency, length)| (doc, scorer.score(frequency, length, 2)))
        .collect();
    let mut blocks = BlockMaxIndex::new(1).unwrap();
    blocks
        .set_block_maxes("docs", "body", "a", expected.values().copied().collect())
        .unwrap();
    for k in [1, 2] {
        let query = WANDQuery::new(
            vec![index.get_posting_list("body", "a").unwrap()],
            vec![scorer.clone()],
            vec!["body".into()],
            vec!["a".into()],
            k,
        )
        .unwrap();
        let query_cursors = CursorWANDQuery::new(
            vec![cursor()],
            vec![scorer.clone()],
            vec!["body".into()],
            vec!["a".into()],
            k,
        )
        .unwrap();
        let results = [
            WANDScorer::new(&query, Some(index)).score_top_k().unwrap(),
            BlockMaxWANDScorer::new(&query, Some(index), &blocks, "docs")
                .score_top_k()
                .unwrap(),
            CursorWANDScorer::new(&query_cursors).score_top_k().unwrap(),
            CursorBlockMaxWANDScorer::new(&query_cursors, &blocks, "docs")
                .score_top_k()
                .unwrap(),
        ];
        for result in results {
            assert_eq!(result.top_k.len(), k);
            for entry in &result.top_k {
                assert!((entry.payload.score - expected[&entry.doc_id]).abs() < 1e-12);
            }
            if k == 1 {
                assert_eq!(result.top_k.entries()[0].doc_id, 2);
            }
        }
    }
}
