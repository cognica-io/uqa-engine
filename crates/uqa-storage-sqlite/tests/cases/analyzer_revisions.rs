//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Immutable provider bindings and atomic publication of replacement postings.

use std::collections::BTreeMap;
use std::sync::Arc;

use uqa_analysis::{whitespace_analyzer, Analyzer, TokenFilter, Tokenizer};
use uqa_storage::{
    AnalyzerPhase, InvertedIndex, KeyValueInvertedIndex, KeyValueStore, MemoryInvertedIndex,
    MemoryKeyValueStore,
};
use uqa_storage_sqlite::{Catalog, ManagedConnection, SQLiteInvertedIndex};

fn connection() -> ManagedConnection {
    let connection = ManagedConnection::open_in_memory().unwrap();
    Catalog::open(connection.clone()).unwrap();
    connection
}

fn providers(default: &Analyzer) -> Vec<Box<dyn InvertedIndex>> {
    let mut providers: Vec<Box<dyn InvertedIndex>> =
        vec![Box::new(MemoryInvertedIndex::new(default.clone()))];
    providers.extend(linear_providers(default));
    providers
}

fn linear_providers(default: &Analyzer) -> Vec<Box<dyn InvertedIndex>> {
    vec![
        Box::new(KeyValueInvertedIndex::new(
            Arc::new(MemoryKeyValueStore::new()),
            "docs",
            default.clone(),
        )),
        Box::new(SQLiteInvertedIndex::new(
            connection(),
            "docs",
            default.clone(),
        )),
    ]
}

fn fields(text: &str) -> BTreeMap<String, String> {
    BTreeMap::from([("body".into(), text.into())])
}

fn synonym_file(path: &std::path::Path) -> Analyzer {
    Analyzer::new(
        Tokenizer::Whitespace,
        vec![TokenFilter::Synonym {
            synonyms: BTreeMap::new(),
            synonyms_path: Some(path.to_str().unwrap().into()),
        }],
        Vec::new(),
    )
}

fn invalid_default() -> Analyzer {
    Analyzer::new(
        Tokenizer::NGram {
            min_gram: 0,
            max_gram: 1,
        },
        Vec::new(),
        Vec::new(),
    )
}

#[test]
fn installed_revisions_survive_file_changes_and_preserve_independent_sides() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("synonyms.txt");
    let config = synonym_file(&path);
    for mut index in providers(&whitespace_analyzer()) {
        std::fs::write(&path, "seed => old\n").unwrap();
        index
            .set_field_analyzer("body", config.clone(), AnalyzerPhase::Both)
            .unwrap();
        index.add_document(1, fields("seed")).unwrap();
        let old = index.index_analyzer_revision("body").unwrap();
        assert!(Arc::ptr_eq(
            &old,
            &index.search_analyzer_revision("body").unwrap()
        ));
        let snapshot = index.snapshot().unwrap();

        std::fs::write(&path, "seed => new\n").unwrap();
        let next = config.compile().unwrap();
        assert_ne!(
            old.descriptor().fingerprint(),
            next.descriptor().fingerprint()
        );
        assert_eq!(config.analyze("seed").unwrap(), ["seed", "new"]);
        std::fs::remove_file(&path).unwrap();
        index.add_document(2, fields("seed")).unwrap();
        assert_eq!(index.doc_freq("body", "old").unwrap(), 2);
        assert_eq!(index.doc_freq("body", "new").unwrap(), 0);
        assert_eq!(
            index.get_search_analyzer("body").analyze("seed").unwrap(),
            ["seed", "old"]
        );

        index
            .rebuild_with_analyzer_revision(
                "body",
                next.clone(),
                AnalyzerPhase::Index,
                vec![(1, fields("seed")), (2, fields("seed"))],
            )
            .unwrap();
        assert_eq!(index.doc_freq("body", "old").unwrap(), 0);
        assert_eq!(index.doc_freq("body", "new").unwrap(), 2);
        assert!(Arc::ptr_eq(
            &next,
            &index.index_analyzer_revision("body").unwrap()
        ));
        assert!(Arc::ptr_eq(
            &old,
            &index.search_analyzer_revision("body").unwrap()
        ));
        assert!(Arc::ptr_eq(
            &old,
            &snapshot.index_analyzer_revision("body").unwrap()
        ));
        index
            .set_field_analyzer_revision("body", next.clone(), AnalyzerPhase::Search)
            .unwrap();
        assert!(Arc::ptr_eq(
            &next,
            &index.search_analyzer_revision("body").unwrap()
        ));
        assert_eq!(index.doc_count().unwrap(), 2);
    }
}

#[test]
fn deferred_defaults_retry_failure_then_keep_one_successful_revision() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("default.txt");
    for mut index in providers(&synonym_file(&path)) {
        assert!(index.add_document(1, fields("seed")).is_err());
        assert_eq!(index.doc_count().unwrap(), 0);
        std::fs::write(&path, "seed => retained\n").unwrap();
        index.add_document(1, fields("seed")).unwrap();
        let revision = index.index_analyzer_revision("body").unwrap();
        std::fs::remove_file(&path).unwrap();
        index.add_document(2, fields("seed")).unwrap();
        assert_eq!(index.doc_freq("body", "retained").unwrap(), 2);
        assert!(Arc::ptr_eq(
            &revision,
            &index.search_analyzer_revision("body").unwrap()
        ));
        assert!(Arc::ptr_eq(
            &revision,
            &index.index_analyzer_revision("another_field").unwrap()
        ));
    }
}

#[test]
fn invalid_assignments_preserve_both_prior_revisions_and_postings() {
    for mut index in providers(&whitespace_analyzer()) {
        index.add_document(1, fields("old")).unwrap();
        let before = index.index_analyzer_revision("body").unwrap();
        for phase in [
            AnalyzerPhase::Index,
            AnalyzerPhase::Search,
            AnalyzerPhase::Both,
        ] {
            assert!(index
                .set_field_analyzer("body", invalid_default(), phase)
                .is_err());
            assert!(Arc::ptr_eq(
                &before,
                &index.index_analyzer_revision("body").unwrap()
            ));
            assert!(Arc::ptr_eq(
                &before,
                &index.search_analyzer_revision("body").unwrap()
            ));
            assert_eq!(index.doc_freq("body", "old").unwrap(), 1);
        }
    }
}

#[test]
fn rebuild_analysis_failure_preserves_bindings_and_the_complete_old_document_set() {
    let next = uqa_analysis::keyword_analyzer().compile().unwrap();
    for mut index in providers(&invalid_default()) {
        index
            .set_field_analyzer("body", whitespace_analyzer(), AnalyzerPhase::Both)
            .unwrap();
        index.add_document(1, fields("old value")).unwrap();
        let before = index.index_analyzer_revision("body").unwrap();
        let error = index
            .rebuild_with_analyzer_revision(
                "body",
                next.clone(),
                AnalyzerPhase::Both,
                vec![
                    (2, fields("new value")),
                    (
                        3,
                        BTreeMap::from([("bad_default".into(), "failure".into())]),
                    ),
                ],
            )
            .unwrap_err();
        assert!(error.to_string().contains("gram"));
        assert_eq!(index.doc_count().unwrap(), 1);
        assert_eq!(
            index
                .get_posting_list("body", "old")
                .unwrap()
                .doc_ids()
                .collect::<Vec<_>>(),
            [1]
        );
        assert_eq!(index.doc_freq("body", "new value").unwrap(), 0);
        assert!(Arc::ptr_eq(
            &before,
            &index.index_analyzer_revision("body").unwrap()
        ));
        assert!(Arc::ptr_eq(
            &before,
            &index.search_analyzer_revision("body").unwrap()
        ));
    }
}

#[test]
fn sqlite_rebuild_storage_failure_rolls_back_deleted_postings_and_new_bindings() {
    let connection = connection();
    let mut index = SQLiteInvertedIndex::new(connection.clone(), "docs", whitespace_analyzer());
    index.add_document(1, fields("old")).unwrap();
    let before = index.index_analyzer_revision("body").unwrap();
    connection.with(|db| Ok(db.execute_batch("CREATE TRIGGER reject_posting BEFORE INSERT ON _posting_clusters BEGIN SELECT RAISE(ABORT, 'forced posting failure'); END;")?)).unwrap();
    let next = uqa_analysis::keyword_analyzer().compile().unwrap();
    let error = index
        .rebuild_with_analyzer_revision(
            "body",
            next,
            AnalyzerPhase::Both,
            vec![(2, fields("new value"))],
        )
        .unwrap_err();
    assert!(error.to_string().contains("forced posting failure"));
    assert_eq!(index.doc_freq("body", "old").unwrap(), 1);
    assert_eq!(index.doc_count().unwrap(), 1);
    assert!(Arc::ptr_eq(
        &before,
        &index.index_analyzer_revision("body").unwrap()
    ));
    assert!(Arc::ptr_eq(
        &before,
        &index.search_analyzer_revision("body").unwrap()
    ));
    connection
        .with(|db| {
            assert!(db.is_autocommit());
            Ok(())
        })
        .unwrap();
}

#[test]
fn key_value_rebuild_write_rejection_preserves_postings_and_bound_revisions() {
    let store = Arc::new(MemoryKeyValueStore::new());
    let mut index = KeyValueInvertedIndex::new(store.clone(), "docs", whitespace_analyzer());
    index.add_document(1, fields("old")).unwrap();
    let before = index.index_analyzer_revision("body").unwrap();
    store.begin_read_transaction().unwrap();
    let next = uqa_analysis::keyword_analyzer().compile().unwrap();
    let error = index
        .rebuild_with_analyzer_revision(
            "body",
            next,
            AnalyzerPhase::Both,
            vec![(2, fields("new value"))],
        )
        .unwrap_err();
    assert!(error.to_string().contains("read-only"));
    assert_eq!(index.doc_freq("body", "old").unwrap(), 1);
    assert_eq!(index.doc_count().unwrap(), 1);
    assert!(Arc::ptr_eq(
        &before,
        &index.index_analyzer_revision("body").unwrap()
    ));
    assert!(Arc::ptr_eq(
        &before,
        &index.search_analyzer_revision("body").unwrap()
    ));
    assert!(!store.transaction_has_written().unwrap());
    store.rollback_transaction().unwrap();
}

#[test]
fn memory_batch_analysis_failure_does_not_publish_earlier_documents() {
    let mut index = MemoryInvertedIndex::new(invalid_default());
    index
        .set_field_analyzer("body", whitespace_analyzer(), AnalyzerPhase::Both)
        .unwrap();
    index.add_document(1, fields("old")).unwrap();
    assert!(index
        .try_add_documents(vec![
            (1, fields("replacement")),
            (2, fields("new")),
            (
                3,
                BTreeMap::from([("bad_default".into(), "failure".into())])
            )
        ])
        .is_err());
    assert_eq!(index.doc_count().unwrap(), 1);
    assert_eq!(index.doc_freq("body", "old").unwrap(), 1);
    assert_eq!(index.doc_freq("body", "replacement").unwrap(), 0);
    assert_eq!(index.doc_freq("body", "new").unwrap(), 0);
}

#[test]
fn linear_providers_reject_normalization_policies_they_cannot_store() {
    let revision = uqa_analysis::AnalyzerResources::default()
        .compile_with_length_policy(
            &whitespace_analyzer(),
            uqa_analysis::TokenLengthPolicy::DiscountOverlaps,
        )
        .unwrap();
    for mut index in linear_providers(&whitespace_analyzer()) {
        index.add_document(1, fields("old")).unwrap();
        let before = index.index_analyzer_revision("body").unwrap();
        assert!(index
            .set_field_analyzer_revision("body", revision.clone(), AnalyzerPhase::Both)
            .unwrap_err()
            .contains("occurrence storage"));
        assert!(index
            .rebuild_with_analyzer_revision(
                "body",
                revision.clone(),
                AnalyzerPhase::Both,
                vec![(2, fields("new"))]
            )
            .unwrap_err()
            .to_string()
            .contains("occurrence storage"));
        assert_eq!(index.doc_freq("body", "old").unwrap(), 1);
        assert!(Arc::ptr_eq(
            &before,
            &index.index_analyzer_revision("body").unwrap()
        ));
        assert!(Arc::ptr_eq(
            &before,
            &index.search_analyzer_revision("body").unwrap()
        ));
    }
}

#[test]
fn concurrent_default_resolution_shares_one_revision_with_cache_retention_disabled() {
    let bindings = Arc::new(
        uqa_storage::inverted_index::AnalyzerBindings::with_resources(
            whitespace_analyzer(),
            uqa_analysis::AnalyzerResources::new(uqa_analysis::AnalyzerLimits {
                max_cached_analyzers: 0,
                ..uqa_analysis::AnalyzerLimits::default()
            }),
        ),
    );
    let barrier = Arc::new(std::sync::Barrier::new(8));
    let threads: Vec<_> = (0..8)
        .map(|_| {
            let bindings = bindings.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                bindings.search_revision("body").unwrap()
            })
        })
        .collect();
    let revisions: Vec<_> = threads
        .into_iter()
        .map(|thread| thread.join().unwrap())
        .collect();
    for revision in &revisions[1..] {
        assert!(Arc::ptr_eq(&revisions[0], revision));
    }
}

#[test]
fn linear_providers_reject_missing_graph_information_and_unpaired_key_projection() {
    let scalar = uqa_storage::TokenTermKey::from_text("�");
    let raw =
        uqa_storage::TokenTermKey::from_term(&uqa_analysis::TokenTerm::from_utf16(vec![0xd83d]));
    for mut index in linear_providers(&whitespace_analyzer()) {
        index.add_document(1, fields("� a")).unwrap();
        assert_eq!(index.doc_freq_key("body", &scalar).unwrap(), 1);
        assert_eq!(index.get_term_freq_key(1, "body", &scalar).unwrap(), 1);
        assert_eq!(
            index.get_posting_list_key("body", &scalar).unwrap().len(),
            1
        );
        assert_eq!(
            index
                .posting_cursor_key("body", &scalar)
                .unwrap()
                .current()
                .unwrap()
                .doc_id,
            1
        );
        assert!(index.vocabulary_keys("body").unwrap().contains(&scalar));
        assert!(!index.vocabulary_keys("body").unwrap().contains(&raw));
        assert!(index.doc_freq_key("body", &raw).is_err());
        assert!(index.get_term_freq_key(1, "body", &raw).is_err());
        assert!(index.get_posting_list_key("body", &raw).is_err());
        assert!(index.posting_cursor_key("body", &raw).is_err());
        assert!(index
            .get_occurrence_postings("body", &scalar)
            .unwrap_err()
            .to_string()
            .contains("not supported"));
        assert!(index.get_occurrences(1, "body", &scalar).is_err());
        assert!(index
            .indexed_field_metadata(1, "body")
            .unwrap_err()
            .to_string()
            .contains("not supported"));
        assert_eq!(index.doc_count().unwrap(), 1);
        assert_eq!(index.get_term_freq(1, "body", "�").unwrap(), 1);
    }
}

#[test]
fn atomic_revision_pair_skips_an_unused_invalid_default() {
    let index_revision = whitespace_analyzer().compile().unwrap();
    let search_revision = uqa_analysis::keyword_analyzer().compile().unwrap();
    for mut index in providers(&invalid_default()) {
        index
            .set_field_analyzer_revisions("body", index_revision.clone(), search_revision.clone())
            .unwrap();
        index.add_document(1, fields("two words")).unwrap();
        assert_eq!(index.doc_freq("body", "two").unwrap(), 1);
        assert!(Arc::ptr_eq(
            &index_revision,
            &index.index_analyzer_revision("body").unwrap()
        ));
        assert!(Arc::ptr_eq(
            &search_revision,
            &index.search_analyzer_revision("body").unwrap()
        ));
    }
}

#[test]
fn unsupported_search_revision_does_not_publish_the_candidate_index_side() {
    let next = uqa_analysis::keyword_analyzer().compile().unwrap();
    let unsupported = uqa_analysis::AnalyzerResources::default()
        .compile_with_length_policy(
            &whitespace_analyzer(),
            uqa_analysis::TokenLengthPolicy::DiscountOverlaps,
        )
        .unwrap();
    for mut index in linear_providers(&whitespace_analyzer()) {
        index.add_document(1, fields("old")).unwrap();
        let before = index.index_analyzer_revision("body").unwrap();
        assert!(index
            .set_field_analyzer_revisions("body", next.clone(), unsupported.clone())
            .is_err());
        assert!(Arc::ptr_eq(
            &before,
            &index.index_analyzer_revision("body").unwrap()
        ));
        assert!(Arc::ptr_eq(
            &before,
            &index.search_analyzer_revision("body").unwrap()
        ));
        assert_eq!(index.doc_freq("body", "old").unwrap(), 1);
    }
}
