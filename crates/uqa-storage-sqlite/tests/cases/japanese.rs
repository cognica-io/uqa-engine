//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use uqa_analysis::whitespace_analyzer;
use uqa_scoring::{
    rebuild_text_block_max, score_text_terms, BM25Params, BM25Scorer, ScoringMode,
    TextSearchAlgorithm,
};
use uqa_storage::InvertedIndex;
use uqa_storage_sqlite::{Catalog, ManagedConnection, SQLiteInvertedIndex};

#[path = "../../../uqa-storage/tests/cases/japanese/contract.rs"]
mod contract;

#[test]
fn japanese_native_occurrences_keep_graphs_and_scorer_bounds_through_rollback() {
    for case in contract::cases() {
        let conn = ManagedConnection::open_in_memory().unwrap();
        Catalog::open(conn.clone()).unwrap();
        conn.bind_native_records(uqa_storage::mvcc::VersionedSessionOptions::default())
            .unwrap();
        let mut index = SQLiteInvertedIndex::new(conn.clone(), "docs", whitespace_analyzer());
        contract::populate(&mut index, &case);
        assert!(rebuild_text_block_max(
            &mut index,
            "body",
            &ScoringMode::BM25(BM25Params::default())
        )
        .unwrap());
        let retained = index.snapshot().unwrap();
        conn.begin_transaction().unwrap();
        index.clear().unwrap();
        conn.rollback_transaction().unwrap();
        for snapshot in [&index as &dyn InvertedIndex, retained.as_ref()] {
            contract::verify(snapshot, &case);
            verify_scores(snapshot, &case);
        }
    }
}

#[test]
fn japanese_occurrences_restore_from_sqlite_backup_with_original_database_removed() {
    for case in contract::cases() {
        let source = tempfile::tempdir().unwrap();
        let backup = tempfile::tempdir().unwrap();
        let source_path = source.path().join("source.db");
        let backup_path = backup.path().join("restored.db");
        {
            let conn = ManagedConnection::open(&source_path).unwrap();
            Catalog::open(conn.clone()).unwrap();
            let mut index = SQLiteInvertedIndex::new(conn, "docs", whitespace_analyzer());
            contract::populate(&mut index, &case);
            contract::verify(&index, &case);
            assert!(rebuild_text_block_max(
                &mut index,
                "body",
                &ScoringMode::BM25(BM25Params::default())
            )
            .unwrap());
            verify_scores(&index, &case);
        }
        std::fs::copy(&source_path, &backup_path).unwrap();
        source.close().unwrap();
        assert!(!source_path.exists());
        let mut index = SQLiteInvertedIndex::new(
            ManagedConnection::open(&backup_path).unwrap(),
            "docs",
            whitespace_analyzer(),
        );
        contract::restore(&mut index, &case);
        contract::verify(&index, &case);
        verify_scores(&index, &case);
    }
}

fn verify_scores(index: &dyn InvertedIndex, case: &contract::JapaneseCase) {
    let scorer = BM25Scorer::new(
        BM25Params::default(),
        std::sync::Arc::new(index.field_stats("body").unwrap()),
    );
    let mut terms = case.expected.terms.keys().cloned().collect::<Vec<_>>();
    terms.push(terms[0].clone());
    let expected: f64 = terms
        .iter()
        .map(|term| {
            scorer.score(
                case.expected.terms[term].len() as u64,
                case.expected.length,
                2,
            )
        })
        .sum();
    for algorithm in [
        TextSearchAlgorithm::Exhaustive,
        TextSearchAlgorithm::Wand,
        TextSearchAlgorithm::BlockMaxWand,
    ] {
        let result = score_text_terms(
            index,
            "docs",
            "body",
            &terms,
            &ScoringMode::BM25(BM25Params::default()),
            2,
            algorithm,
        )
        .unwrap();
        assert_eq!(result.algorithm, algorithm);
        assert_eq!(
            result
                .entries
                .iter()
                .map(|row| row.doc_id)
                .collect::<Vec<_>>(),
            [7, 65_536]
        );
        for row in result.entries {
            assert!((row.score - expected).abs() < 1e-12, "{}", case.id);
        }
    }
}
