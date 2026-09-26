//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Public retrieval preserves document scores and composition across physical index selection.

use super::{kind, sessions, sql, Engine, Value};
use crate::{HybridSearchParams, RobustHybridSearchParams};
use uqa_core::ScoredEntry;
use uqa_sql::SQLResult;

const ALL: &[(i64, f64)] = &[(1, 1.0), (5, 1.0), (2, 0.0), (4, 0.0), (3, -1.0)];
const KNN: &str = "knn_match(embedding, ARRAY[1.0,0.0], 99)";

fn owners(mut exercise: impl FnMut(Engine)) {
    exercise(Engine::new());
    for provider in 0..3 {
        let (_directory, engine, peer) = sessions(provider);
        drop(peer);
        exercise(engine);
    }
}

fn populate(engine: &Engine) {
    sql(engine, "CREATE TABLE diskann_docs(id int PRIMARY KEY, body text, keep boolean, embedding tensor(2)); CREATE INDEX body_idx ON diskann_docs USING gin(body)");
    sql(engine, "INSERT INTO diskann_docs VALUES (1,'alpha one',false,ARRAY[ARRAY[1.0,0.0],ARRAY[0.0,1.0]]),(2,'beta two',true,ARRAY[ARRAY[0.0,1.0],ARRAY[-1.0,0.0]]),(3,'alpha three',true,ARRAY[ARRAY[-1.0,0.0]]),(4,'beta four',true,ARRAY[ARRAY[0.0,0.0]]),(5,'gamma five',true,ARRAY[ARRAY[1.0,0.0]]),(6,'alpha six',true,NULL)");
}

fn create(engine: &Engine) {
    sql(engine, "CREATE INDEX diskann_idx ON diskann_docs USING diskann(embedding) WITH (max_degree=2, search_list_size=2, beam_width=1)");
    assert_eq!(kind(engine), "diskann");
}

fn selected_kind(engine: &Engine) -> String {
    engine
        .require_query_table("diskann_docs")
        .unwrap()
        .vector_indexes
        .read()
        .get("embedding")
        .unwrap()
        .index_kind()
        .to_owned()
}

fn query(engine: &Engine, predicate: &str) -> SQLResult {
    sql(
        engine,
        &format!("SELECT id, _score FROM diskann_docs WHERE {predicate} ORDER BY _score DESC, id"),
    )
}

fn scores(result: &SQLResult) -> Vec<(i64, f64)> {
    result
        .rows
        .iter()
        .map(|row| {
            let Value::Int(id) = row["id"] else {
                panic!("integer id required")
            };
            let Value::Float(score) = row["_score"] else {
                panic!("floating score required")
            };
            (id, score)
        })
        .collect()
}

fn direct_scores(engine: &Engine, entries: Vec<ScoredEntry>) -> Vec<(i64, f64)> {
    let identities = sql(engine, "SELECT id, _doc_id FROM diskann_docs");
    entries
        .into_iter()
        .map(|entry| {
            let row = identities
                .rows
                .iter()
                .find(|row| row["_doc_id"] == Value::Int(entry.doc_id as i64))
                .unwrap();
            let Value::Int(id) = row["id"] else {
                panic!("integer id required")
            };
            (id, entry.score)
        })
        .collect()
}

fn raw_consumers(engine: &Engine) {
    assert_eq!(scores(&query(engine, KNN)), ALL);
    assert_eq!(
        direct_scores(
            engine,
            engine
                .knn_search("diskann_docs", "embedding", [1.0, 0.0], 99)
                .unwrap()
        ),
        ALL
    );
    assert_eq!(
        direct_scores(
            engine,
            engine
                .vector_similarity_search("diskann_docs", "embedding", vec![1.0, 0.0], 0.0)
                .unwrap()
        ),
        &ALL[..4]
    );
    let selected = scores(&query(engine, "knn_match(embedding, ARRAY[1.0,0.0], 3)"));
    assert_eq!(selected.len(), 3);
    assert!(selected.iter().all(|entry| ALL.contains(entry)));
    assert!(selected
        .windows(2)
        .all(|pair| pair[0].1 > pair[1].1 || (pair[0].1 == pair[1].1 && pair[0].0 < pair[1].0)));
    assert_eq!(
        scores(&query(engine, "knn_match(embedding, ARRAY[1.0,0.0], 3)")),
        selected
    );
    assert_eq!(
        scores(&query(
            engine,
            "knn_match(embedding, ARRAY[1.0,0.0], 3) AND keep"
        )),
        selected
            .into_iter()
            .filter(|&(id, _)| id != 1)
            .collect::<Vec<_>>()
    );
    let first = scores(&query(engine, "knn_match(embedding, ARRAY[1.0,0.0], 1)"))[0].0;
    assert!(query(
        engine,
        &format!("knn_match(embedding, ARRAY[1.0,0.0], 1) AND id<>{first}")
    )
    .rows
    .is_empty());
    assert_eq!(
        direct_scores(
            engine,
            engine
                .knn_search("diskann_docs", "embedding", [0.0, 0.0], 99)
                .unwrap()
        ),
        [(1, 0.0), (2, 0.0), (3, 0.0), (4, 0.0), (5, 0.0)]
    );
}

#[test]
fn diskann_public_scores_counts_thresholds_and_filters_preserve_exact_semantics() {
    owners(|engine| {
        let engine = &engine;
        populate(engine);
        raw_consumers(engine);
        create(engine);
        for _ in 0..2 {
            raw_consumers(engine);
        }
        sql(engine, "DELETE FROM diskann_docs");
        assert!(query(engine, KNN).rows.is_empty());
        assert!(engine
            .vector_similarity_search("diskann_docs", "embedding", vec![1.0, 0.0], -1.0)
            .unwrap()
            .is_empty());
        sql(
            engine,
            "INSERT INTO diskann_docs VALUES(7,'alpha',true,ARRAY[ARRAY[-1.0,0.0],ARRAY[0.0,1.0]])",
        );
        assert_eq!(scores(&query(engine, KNN)), [(7, 0.0)]);
        sql(engine, "UPDATE diskann_docs SET embedding=NULL");
        assert!(query(engine, KNN).rows.is_empty());
    });
}

fn hybrid(engine: &Engine) -> Vec<ScoredEntry> {
    engine
        .hybrid_search(&HybridSearchParams {
            table: "diskann_docs",
            text_field: "body",
            text_query: "alpha",
            vector_field: "embedding",
            query_vector: vec![1.0, 0.0],
            knn_pool: 5,
            top_k: 99,
        })
        .unwrap()
}

fn robust(engine: &Engine) -> Vec<ScoredEntry> {
    engine
        .robust_hybrid_search(&RobustHybridSearchParams {
            table: "diskann_docs",
            text_field: "body",
            text_query: "alpha",
            vector_field: "embedding",
            query_vector: vec![1.0, 0.0],
            knn_pool: 5,
            alpha: 0.5,
            top_k: 99,
        })
        .unwrap()
}

#[test]
fn diskann_public_hybrid_pool_and_nested_consumers_preserve_existing_compositions() {
    owners(|engine| {
        let engine = &engine;
        populate(engine);
        let predicates = [
            "calibrated_vector_match(embedding, ARRAY[1.0,0.0], 5)",
            "text_match(body,'alpha') AND knn_match(embedding, ARRAY[1.0,0.0], 5)",
            "fuse_bayesian_evidence(bayesian_match(body,'alpha'),knn_match(embedding, ARRAY[1.0,0.0], 5))",
            "pool_positive_evidence(bayesian_match(body,'alpha'),knn_match(embedding, ARRAY[1.0,0.0], 5))",
            "text_match(body,'alpha') AND knn_match(embedding, ARRAY[1.0,0.0], 5) AND keep",
        ];
        let expected = predicates.map(|predicate| query(engine, predicate));
        let expected_hybrid = direct_scores(engine, hybrid(engine));
        let expected_robust = direct_scores(engine, robust(engine));
        assert_eq!(expected[1].rows, expected[2].rows);
        assert_eq!(scores(&expected[1]), expected_hybrid);
        assert_eq!(
            expected[1].rows.len(),
            6,
            "text-only support must survive fusion"
        );
        assert_eq!(
            expected[4].rows.len(),
            5,
            "ordinary conjunct filters fused support"
        );
        create(engine);
        for _ in 0..2 {
            for (predicate, expected) in predicates.iter().zip(&expected) {
                assert_eq!(query(engine, predicate).rows, expected.rows, "{predicate}");
            }
            assert_eq!(direct_scores(engine, hybrid(engine)), expected_hybrid);
            assert_eq!(direct_scores(engine, robust(engine)), expected_robust);
        }
        for statement in [
            "WITH hits AS MATERIALIZED (SELECT id,_score FROM diskann_docs WHERE knn_match(embedding,ARRAY[1.0,0.0],99)) SELECT * FROM hits ORDER BY _score DESC,id",
            "SELECT * FROM (SELECT id,_score FROM diskann_docs WHERE knn_match(embedding,ARRAY[1.0,0.0],99)) hits ORDER BY _score DESC,id",
        ] {
            assert_eq!(scores(&sql(engine, statement)), ALL);
        }
        sql(engine, "CREATE VIEW ranked_docs WITH (security_barrier=true) AS SELECT id,_score FROM diskann_docs WHERE knn_match(embedding,ARRAY[1.0,0.0],2)");
        assert_eq!(
            scores(&sql(
                engine,
                "SELECT * FROM ranked_docs WHERE id>1 ORDER BY _score DESC,id"
            )),
            [(5, 1.0)]
        );
    });
}

#[test]
fn diskann_public_cursors_preserve_results_across_mutation_and_index_retirement() {
    owners(|engine| {
        let engine = &engine;
        populate(engine);
        create(engine);
        let cursor = engine
            .sql_cursor(
                &format!("SELECT id,_score FROM diskann_docs WHERE {KNN} ORDER BY _score DESC,id"),
                &[],
            )
            .unwrap();
        sql(engine, &format!("BEGIN; DECLARE saved NO SCROLL CURSOR WITH HOLD FOR SELECT id,_score FROM diskann_docs WHERE {KNN} ORDER BY _score DESC,id"));
        sql(engine, "UPDATE diskann_docs SET embedding=ARRAY[ARRAY[-1.0,0.0]] WHERE id=1; DELETE FROM diskann_docs WHERE id=5");
        assert_eq!(
            scores(&sql(engine, "FETCH FORWARD 2 FROM saved")),
            &ALL[..2]
        );
        sql(
            engine,
            "COMMIT; DROP INDEX diskann_idx; DELETE FROM diskann_docs",
        );
        assert_eq!(scores(&sql(engine, "FETCH ALL FROM saved")), &ALL[2..]);
        sql(engine, "CLOSE saved");
        let actual = cursor
            .flat_map(|batch| {
                let batch = batch.unwrap();
                batch.columns()[0]
                    .values
                    .iter()
                    .zip(&batch.columns()[1].values)
                    .map(|(id, score)| {
                        let (Value::Int(id), Value::Float(score)) = (id, score) else {
                            panic!("typed cursor row required")
                        };
                        (*id, *score)
                    })
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        assert_eq!(actual, ALL);
    });
}

#[test]
fn diskann_nested_statement_copies_keep_physical_readers_after_source_closure() {
    owners(|source| {
        let engine = &source;
        populate(engine);
        create(engine);
        let snapshot = engine.capture_statement_read_snapshot().unwrap();
        let retained = engine.statement_read_snapshot_engine(&snapshot);
        assert_eq!(selected_kind(&retained), "diskann");
        sql(engine, "UPDATE diskann_docs SET embedding=ARRAY[ARRAY[-1.0,0.0]] WHERE id=1; DROP INDEX diskann_idx; DELETE FROM diskann_docs");
        let nested_snapshot = retained.capture_statement_read_snapshot().unwrap();
        let nested = retained.statement_read_snapshot_engine(&nested_snapshot);
        assert_eq!(selected_kind(&nested), "diskann");
        assert_eq!(
            sql(
                &nested,
                "SELECT indexname FROM pg_indexes WHERE indexname='diskann_idx'"
            )
            .rows
            .len(),
            1
        );
        engine.close().unwrap();
        drop(source);
        drop((snapshot, retained, nested_snapshot));
        assert_eq!(scores(&query(&nested, KNN)), ALL);
        assert_eq!(
            direct_scores(
                &nested,
                nested
                    .vector_similarity_search("diskann_docs", "embedding", vec![1.0, 0.0], 0.0)
                    .unwrap()
            ),
            &ALL[..4]
        );
    });
}

#[test]
fn diskann_private_repeatable_read_copies_keep_physical_selection_and_original_rows() {
    owners(|engine| {
        populate(&engine);
        create(&engine);
        sql(&engine, "BEGIN ISOLATION LEVEL REPEATABLE READ");
        assert_eq!(scores(&query(&engine, KNN)), ALL);
        sql(
            &engine,
            "UPDATE diskann_docs SET embedding=ARRAY[ARRAY[-1.0,0.0]] WHERE id=1; DELETE FROM diskann_docs WHERE id=2; UPDATE diskann_docs SET embedding=NULL WHERE id=5; INSERT INTO diskann_docs VALUES(7,'private seven',true,ARRAY[ARRAY[0.0,1.0],ARRAY[1.0,0.0]])",
        );
        let snapshot = engine.capture_statement_read_snapshot().unwrap();
        let reader = engine.statement_read_snapshot_engine(&snapshot);
        assert_eq!(selected_kind(&reader), "diskann");
        assert_eq!(
            reader
                .require_query_table("diskann_docs")
                .unwrap()
                .vector_indexes
                .read()
                .get("embedding")
                .unwrap()
                .count()
                .unwrap(),
            5
        );
        sql(&engine, "ROLLBACK");
        let expected = [(7, 1.0), (4, 0.0), (1, -1.0), (3, -1.0)];
        assert_eq!(scores(&query(&reader, KNN)), expected);
        assert_eq!(scores(&query(&engine, KNN)), ALL);
        let copied = reader.capture_statement_read_snapshot().unwrap();
        let nested = reader.statement_read_snapshot_engine(&copied);
        assert_eq!(selected_kind(&nested), "diskann");
        assert_eq!(scores(&query(&nested, KNN)), expected);
        engine.close().unwrap();
        drop((engine, reader, snapshot, copied));
        assert_eq!(scores(&query(&nested, KNN)), expected);
        assert_eq!(
            direct_scores(
                &nested,
                nested
                    .vector_similarity_search("diskann_docs", "embedding", vec![1.0, 0.0], 0.0)
                    .unwrap()
            ),
            expected[..2]
        );
    });
}
