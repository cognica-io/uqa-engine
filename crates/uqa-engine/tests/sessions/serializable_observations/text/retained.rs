//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Nested text readers keep their original participant, budget and cancellation.

use super::{finish, fixtures, index, pivot, prepare, search};
use std::sync::Arc;
use uqa_core::CancellationToken;
use uqa_execution::{
    serializable::{text::ObservedTextIndex, SerializableRelationRead},
    storage_errors::storage_error,
};
use uqa_storage::{InvertedIndex, TokenTermKey};

#[test]
fn admitted_nested_text_snapshots_keep_old_values_and_original_participants() {
    let (_directory, sessions) = fixtures();
    for a in sessions {
        prepare(&a);
        let b = a.sibling();
        a.begin();
        b.begin();
        let retained = index(&a).snapshot().unwrap().snapshot().unwrap();
        b.sql("UPDATE texts SET body = 'omega omega' WHERE id = 1");
        assert_eq!(retained.get_posting_list("body", "alpha").unwrap().len(), 1);
        assert_eq!(retained.get_doc_length(1, "body").unwrap(), 2);
        pivot(&a, &b);
        finish(&a, &b, true);
        a.begin();
        assert!(retained.get_posting_list("body", "alpha").is_err());
        assert_eq!(search(&a, "postings", "alpha"), 1);
        a.engine.rollback().unwrap();
    }
}

#[test]
fn text_readers_share_the_original_memory_limit_without_observing_metadata() {
    let (_directory, sessions) = fixtures();
    for a in sessions {
        prepare(&a);
        a.begin();
        let context = a
            .backend
            .serializable_session()
            .unwrap()
            .serializable_read_context()
            .unwrap()
            .unwrap();
        let cancellation = CancellationToken::new();
        let control = context.read_control(&cancellation);
        // This wrapper deliberately has no SQL schema: its dynamic address and all scratch space still use the original participant's allowance.
        let read = SerializableRelationRead::new([81; 16], context, &cancellation);
        let reader = ObservedTextIndex::new(index(&a), Some(read), Arc::new(Vec::new()));
        let occupied = control
            .memory()
            .reserve(control.memory().limit() - control.memory().used())
            .unwrap();
        reader.search_analyzer_revision("body").unwrap();
        let error = reader.doc_freq("body", "alpha").unwrap_err();
        assert_eq!(storage_error("read text", &error).sqlstate(), Some("53200"));
        drop(occupied);
        assert_eq!(reader.doc_freq("body", "alpha").unwrap(), 1);
        cancellation.cancel();
        let error = reader
            .get_posting_list_key("body", &TokenTermKey::from_text("alpha"))
            .unwrap_err();
        assert_eq!(storage_error("read text", &error).sqlstate(), Some("57014"));
        a.engine.rollback().unwrap();
    }
}

#[test]
fn controlled_phrase_and_score_reads_preserve_serialization_failure_codes() {
    for route in ["sql", "phrase", "wand", "bmw"] {
        let (_directory, sessions) = fixtures();
        for a in sessions {
            prepare(&a);
            let b = a.sibling();
            a.begin();
            b.begin();
            assert_eq!(search(&a, "postings", "alpha"), 1);
            let retained = index(&b);
            pivot(&a, &b);
            b.sql("INSERT INTO texts VALUES (3, 'alpha', NULL)");
            a.engine.commit().unwrap();
            let result = if route == "sql" {
                b.engine
                    .sql("SELECT id FROM texts WHERE text_match(body, 'alpha')", &[])
                    .map(|_| ())
            } else {
                let tree = if route == "phrase" {
                    super::OperatorTree::Phrase {
                        query: "alpha beta".into(),
                        field: Some("body".into()),
                        scoring: Some(super::TextScoringMode::BM25),
                    }
                } else {
                    super::OperatorTree::Term {
                        query: "alpha".into(),
                        field: Some("body".into()),
                        scoring: Some(super::TextScoringMode::BM25),
                        top_k: Some(super::TextTopKPlan {
                            k: 1,
                            strategy: if route == "wand" {
                                super::TextTopKStrategy::Wand
                            } else {
                                super::TextTopKStrategy::BlockMaxWand
                            },
                        }),
                    }
                };
                super::OperatorTreeDriver::execute_node(
                    &super::EngineDriver::new(&b.engine, "texts", &[]),
                    &tree,
                )
                .map(|_| ())
            };
            assert_eq!(result.unwrap_err().sqlstate(), Some("40001"), "{route}");
            let error = retained.field_stats_scalar("body").unwrap_err();
            assert_eq!(
                storage_error("retained text", &error).sqlstate(),
                Some("40001")
            );
            b.engine.rollback().unwrap();
        }
    }
}
