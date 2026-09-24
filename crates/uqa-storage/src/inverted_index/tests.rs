//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_analysis::analyzer::standard_analyzer;

fn fields<const N: usize>(pairs: [(&str, &str); N]) -> BTreeMap<FieldName, String> {
    pairs
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

#[test]
fn add_document_indexes_tokens() {
    let mut idx = MemoryInvertedIndex::new(standard_analyzer("english"));
    idx.add_document(1, fields([("title", "The Rust Programming Language")]))
        .unwrap();
    idx.add_document(2, fields([("title", "Programming with Rust")]))
        .unwrap();

    let pl = idx.get_posting_list("title", "rust").unwrap();
    let docs: Vec<_> = pl.doc_ids().collect();
    assert_eq!(docs, vec![1, 2]);

    // standard analyzer stems "programming" -> "program"
    let pl2 = idx.get_posting_list("title", "program").unwrap();
    let docs2: Vec<_> = pl2.doc_ids().collect();
    assert_eq!(docs2, vec![1, 2]);
}

#[test]
fn snapshot_creation_does_not_scale_with_document_count() {
    let allocations = [1, 1024].map(|count| {
        let mut index = MemoryInvertedIndex::new(uqa_analysis::whitespace_analyzer());
        for id in 0..count {
            index
                .add_document(id, fields([("body", "snapshot payload repeated tokens")]))
                .unwrap();
        }
        let mut snapshots = None;
        let allocation = allocation_counter::measure(|| {
            snapshots = Some((
                index.snapshot().unwrap(),
                index.writable_snapshot().unwrap(),
            ));
        });
        let (read, writable) = snapshots.unwrap();
        index.clear().unwrap();
        assert_eq!(read.doc_count().unwrap(), count);
        assert_eq!(writable.doc_count().unwrap(), count);
        allocation
    });
    assert_eq!(allocations[0].bytes_total, allocations[1].bytes_total);
    assert_eq!(allocations[0].count_total, allocations[1].count_total);
}

#[test]
fn writable_snapshots_keep_postings_counters_and_revisions_independent() {
    let mut index = MemoryInvertedIndex::new(uqa_analysis::whitespace_analyzer());
    index
        .add_document(1, fields([("body", "old shared")]))
        .unwrap();
    index.add_document(2, fields([("body", "shared")])).unwrap();
    let read = index.snapshot().unwrap();
    let mut writable = index.writable_snapshot().unwrap();
    let original_revision = index.search_analyzer_revision("body").unwrap();
    let key = TokenTermKey::from_text("shared");
    let original_occurrences = index.get_occurrences(1, "body", &key).unwrap();

    writable.remove_document(2).unwrap();
    writable
        .try_add_documents(vec![
            (1, fields([("body", "next next")])),
            (3, fields([("body", "next")])),
        ])
        .unwrap();
    writable
        .set_field_analyzer("body", standard_analyzer("english"), AnalyzerPhase::Search)
        .unwrap();
    index.add_document(1, fields([("body", "source")])).unwrap();

    assert_eq!(read.doc_freq("body", "shared").unwrap(), 2);
    assert_eq!(read.total_field_length("body").unwrap(), 3);
    assert_eq!(
        read.get_occurrences(1, "body", &key).unwrap(),
        original_occurrences
    );
    assert!(Arc::ptr_eq(
        &read.search_analyzer_revision("body").unwrap(),
        &original_revision
    ));
    assert_eq!(writable.doc_freq("body", "next").unwrap(), 2);
    assert_eq!(writable.total_field_length("body").unwrap(), 3);
    assert_eq!(writable.field_doc_count("body").unwrap(), 2);
    assert!(writable
        .indexed_field_metadata(2, "body")
        .unwrap()
        .is_none());
    assert_eq!(index.doc_freq("body", "shared").unwrap(), 1);
    assert_eq!(index.doc_freq("body", "next").unwrap(), 0);
    assert!(Arc::ptr_eq(
        &index.search_analyzer_revision("body").unwrap(),
        &original_revision
    ));

    let second = writable.snapshot().unwrap();
    writable.clear().unwrap();
    writable
        .add_document(9, fields([("body", "reused")]))
        .unwrap();
    index.clear().unwrap();
    assert_eq!(second.doc_freq("body", "next").unwrap(), 2);
    assert_eq!(read.doc_freq("body", "shared").unwrap(), 2);
    assert_eq!(writable.doc_count().unwrap(), 1);
    assert_eq!(writable.doc_freq("body", "reused").unwrap(), 1);
}

#[test]
fn doc_freq_counts_documents() {
    let mut idx = MemoryInvertedIndex::new(standard_analyzer("english"));
    idx.add_document(1, fields([("title", "rust")])).unwrap();
    idx.add_document(2, fields([("title", "rust rust rust")]))
        .unwrap();
    idx.add_document(3, fields([("title", "go")])).unwrap();

    assert_eq!(idx.doc_freq("title", "rust").unwrap(), 2);
    assert_eq!(idx.doc_freq("title", "go").unwrap(), 1);
    assert_eq!(idx.doc_freq("title", "java").unwrap(), 0);
}

#[test]
fn term_freq_counts_positions() {
    let mut idx = MemoryInvertedIndex::new(standard_analyzer("english"));
    idx.add_document(1, fields([("title", "rust rust rust")]))
        .unwrap();
    // After standard analyzer: ["rust", "rust", "rust"]
    assert_eq!(idx.get_term_freq(1, "title", "rust").unwrap(), 3);
}

#[test]
fn doc_length_tracks_token_count() {
    let mut idx = MemoryInvertedIndex::new(standard_analyzer("english"));
    // standard analyzer drops "the" / "is" stop words
    idx.add_document(1, fields([("title", "the rust language is fast")]))
        .unwrap();
    // Remaining tokens: ["rust", "languag", "fast"] -> 3
    assert_eq!(idx.get_doc_length(1, "title").unwrap(), 3);
}

#[test]
fn remove_document_clears_postings_and_lengths() {
    let mut idx = MemoryInvertedIndex::new(standard_analyzer("english"));
    idx.add_document(1, fields([("title", "rust")])).unwrap();
    idx.add_document(2, fields([("title", "rust")])).unwrap();
    idx.remove_document(1).unwrap();

    assert_eq!(idx.doc_freq("title", "rust").unwrap(), 1);
    assert_eq!(idx.get_doc_length(1, "title").unwrap(), 0);
    assert_eq!(idx.doc_count().unwrap(), 1);
}

#[test]
fn replacing_doc_replaces_postings() {
    let mut idx = MemoryInvertedIndex::new(standard_analyzer("english"));
    idx.add_document(1, fields([("title", "rust")])).unwrap();
    idx.add_document(1, fields([("title", "go")])).unwrap();

    assert_eq!(idx.doc_freq("title", "rust").unwrap(), 0);
    assert_eq!(idx.doc_freq("title", "go").unwrap(), 1);
    assert_eq!(idx.doc_count().unwrap(), 1);
}

#[test]
fn empty_field_map_removes_existing_index_document() {
    let mut idx = MemoryInvertedIndex::new(standard_analyzer("english"));
    idx.add_document(1, fields([("title", "rust")])).unwrap();
    idx.add_document(1, BTreeMap::new()).unwrap();
    idx.add_document(2, BTreeMap::new()).unwrap();

    assert_eq!(idx.doc_count().unwrap(), 0);
    assert_eq!(idx.doc_freq("title", "rust").unwrap(), 0);
    assert!(!idx.state.documents.contains_key(&1));
    assert!(!idx.state.documents.contains_key(&2));
}

#[test]
fn stats_avg_doc_length_correct() {
    let mut idx = MemoryInvertedIndex::new(standard_analyzer("english"));
    idx.add_document(1, fields([("title", "rust language")]))
        .unwrap();
    idx.add_document(2, fields([("title", "rust")])).unwrap();
    let s = idx.stats().unwrap();
    assert_eq!(s.total_docs, 2);
    // 2 + 1 = 3 tokens / 2 docs = 1.5
    assert!((s.avg_doc_length - 1.5).abs() < 1e-9);
    assert_eq!(s.doc_freq("title", "rust"), 2);
}

#[test]
fn token_position_format_requires_a_representable_end() {
    let mut occurrence = uqa_core::TokenOccurrence {
        position: u32::MAX - 1,
        position_length: 1,
        offsets: None,
    };
    occurrence.validate().unwrap();
    occurrence.position = u32::MAX;
    assert!(occurrence.validate().is_err());
}

#[test]
fn add_overflow_does_not_partially_insert_document() {
    let mut idx = MemoryInvertedIndex::new(standard_analyzer("english"));
    Arc::make_mut(&mut idx.state).doc_count = u64::MAX;

    let error = idx
        .add_document(7, fields([("title", "rust")]))
        .unwrap_err();
    assert!(error.to_string().contains("document count"));
    assert_eq!(idx.state.doc_count, u64::MAX);
    assert!(!idx.state.documents.contains_key(&7));
    assert!(idx.get_posting_list("title", "rust").unwrap().is_empty());
}

#[test]
fn field_length_overflow_preserves_existing_document() {
    let mut idx = MemoryInvertedIndex::new(standard_analyzer("english"));
    idx.add_document(1, fields([("title", "rust")])).unwrap();
    Arc::make_mut(&mut idx.state)
        .field_counters
        .get_mut("title")
        .unwrap()
        .total = u64::MAX;

    let error = idx.add_document(2, fields([("title", "go")])).unwrap_err();
    assert!(error.to_string().contains("total field length"));
    assert_eq!(idx.doc_count().unwrap(), 1);
    assert_eq!(idx.doc_freq("title", "rust").unwrap(), 1);
    assert_eq!(idx.doc_freq("title", "go").unwrap(), 0);
    assert!(!idx.state.documents.contains_key(&2));
}

#[test]
fn corrupt_counter_rejects_remove_without_mutating_postings() {
    let mut idx = MemoryInvertedIndex::new(standard_analyzer("english"));
    idx.add_document(1, fields([("title", "rust")])).unwrap();
    Arc::make_mut(&mut idx.state)
        .field_counters
        .get_mut("title")
        .unwrap()
        .total = 0;

    let error = idx.remove_document(1).unwrap_err();
    assert!(error.to_string().contains("total field length"));
    assert_eq!(idx.doc_count().unwrap(), 1);
    assert_eq!(idx.doc_freq("title", "rust").unwrap(), 1);
    assert_eq!(idx.get_doc_length(1, "title").unwrap(), 1);
}

#[test]
fn stats_reports_cross_field_total_overflow() {
    let mut idx = MemoryInvertedIndex::new(standard_analyzer("english"));
    Arc::make_mut(&mut idx.state).doc_count = 1;
    Arc::make_mut(&mut idx.state).field_counters.insert(
        "a".into(),
        MemoryFieldCounters {
            total: u64::MAX,
            docs: 1,
        },
    );
    Arc::make_mut(&mut idx.state)
        .field_counters
        .insert("b".into(), MemoryFieldCounters { total: 1, docs: 1 });

    let error = idx.stats().unwrap_err();
    assert!(error.to_string().contains("total document length"));
}

#[test]
fn replacement_merges_borrowed_field_names_once_in_order() {
    let mut index = MemoryInvertedIndex::new(uqa_analysis::whitespace_analyzer());
    index
        .add_document(1, fields([("a", "old"), ("c", "old old"), ("e", "old")]))
        .unwrap();
    index.add_document(2, fields([("c", "other")])).unwrap();
    let staged = index
        .stage_document(1, fields([("b", "new"), ("c", "new"), ("d", "new new")]))
        .unwrap();
    let mut copied = Vec::new();
    let plan = index
        .state
        .plan_replacement_with_names(1, &staged.fields, |field| {
            copied.push(field.clone());
            Ok(field.clone())
        })
        .unwrap();
    assert_eq!(copied, ["a", "b", "c", "d", "e"]);
    assert_eq!(
        plan.field_counters
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["a", "b", "c", "d", "e"]
    );
    Arc::make_mut(&mut index.state)
        .apply_replacement(1, staged, plan)
        .unwrap();
    assert_eq!(index.field_names().unwrap(), ["b", "c", "d"]);
    assert_eq!(index.total_field_length("c").unwrap(), 2);
    assert_eq!(index.field_doc_count("c").unwrap(), 2);
    assert_eq!(index.get_term_freq(2, "c", "other").unwrap(), 1);
    super::snapshot::tests::assert_accounted(&index);
}
