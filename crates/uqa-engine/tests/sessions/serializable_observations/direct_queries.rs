//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Direct retrieval and graph queries share SQL admission, retained views and failure cleanup.

use super::admission::{observed_fixture, participant, pending_snapshot};
use super::*;
use std::collections::BTreeMap;
use uqa_core::{Value, Vertex};
use uqa_engine::{HybridSearchParams, RobustHybridSearchParams};
use uqa_graph::GraphStore;
use uqa_scoring::{
    ScoringMode, VectorCalibrationModel, VectorCalibrationProvenance, VectorCalibrationTarget,
    VectorProbabilityTransform,
};
use uqa_sql::SQLError;
use uqa_storage::mvcc::SerializableTransactionId;

fn prepare(engine: &Engine) {
    engine.sql("CREATE TABLE docs (id INTEGER PRIMARY KEY, body TEXT, embedding VECTOR(2)); CREATE INDEX docs_body ON docs USING gin(body); INSERT INTO docs VALUES (1, 'indexed document', ARRAY[1.0, 0.0])", &[]).unwrap();
    engine.bayesian_params_for("docs", "body").unwrap();
    engine.create_graph("g").unwrap();
    engine.add_graph_vertex(Vertex::new(1, "P"), "g").unwrap();
    engine
        .build_path_index("p", "g", &[vec!["knows".into()]])
        .unwrap();
}

fn graph_error(error: impl std::error::Error + Send + Sync + 'static) -> SQLError {
    uqa_execution::storage_errors::storage_error(
        "direct graph query",
        &uqa_storage::StorageBackendError::backend("graph", error),
    )
}

#[derive(Clone, Copy, Debug)]
enum Query {
    Text,
    Profiled,
    Parameters,
    Calibration,
    Knn,
    VectorSimilarity,
    VectorModel,
    Hybrid,
    RobustHybrid,
    MissingModel,
    Physical,
    Graph,
    Cypher,
    GraphExists,
    GraphNames,
    GraphLabels,
    GraphCatalog,
    Path,
    PathNames,
}

impl Query {
    const ALL: [Self; 19] = [
        Self::Text,
        Self::Profiled,
        Self::Parameters,
        Self::Calibration,
        Self::Knn,
        Self::VectorSimilarity,
        Self::VectorModel,
        Self::Hybrid,
        Self::RobustHybrid,
        Self::MissingModel,
        Self::Physical,
        Self::Graph,
        Self::Cypher,
        Self::GraphExists,
        Self::GraphNames,
        Self::GraphLabels,
        Self::GraphCatalog,
        Self::Path,
        Self::PathNames,
    ];

    fn run(self, engine: &Engine, provider: usize, table: &str) -> Result<(), SQLError> {
        let mode = ScoringMode::BM25(uqa_scoring::BM25Params::default());
        match self {
            Self::Text => {
                assert_eq!(engine.search(table, "body", "indexed", &mode, 5)?.len(), 1);
            }
            Self::Profiled => {
                assert_eq!(
                    engine
                        .search_profiled(table, "body", "indexed", &mode, 5)?
                        .entries
                        .len(),
                    1
                );
            }
            Self::Parameters => {
                engine.bayesian_params_for(table, "body")?;
            }
            Self::Calibration => {
                engine.calibration_report(table, "body", "indexed", &[1])?;
            }
            Self::Knn => {
                assert_eq!(
                    engine.knn_search(table, "embedding", [1.0, 0.0], 5)?.len(),
                    1
                );
            }
            Self::VectorSimilarity => {
                assert_eq!(
                    engine
                        .vector_similarity_search(table, "embedding", vec![1.0, 0.0], 0.5)?
                        .len(),
                    1
                );
            }
            Self::VectorModel => {
                assert_eq!(calibrated_vectors(engine, provider, table)?, 1);
            }
            Self::Hybrid | Self::RobustHybrid => {
                assert_eq!(
                    hybrid_search(engine, table, matches!(self, Self::RobustHybrid))?,
                    1
                );
            }
            Self::MissingModel => {
                assert!(engine.deep_predict("missing_model")?.is_none());
            }
            Self::Physical => {
                let output =
                    EngineDriver::new(engine, table, &[]).execute_node(&OperatorTree::KNN {
                        query_vector: vec![1.0, 0.0],
                        k: 5,
                        field: "embedding".into(),
                    })?;
                let OperatorOutput::Posting(postings) = output else {
                    panic!("expected postings")
                };
                assert_eq!(postings.len(), 1);
            }
            Self::Graph
            | Self::Cypher
            | Self::GraphExists
            | Self::GraphNames
            | Self::GraphLabels
            | Self::GraphCatalog
            | Self::Path
            | Self::PathNames => {
                self.run_graph(engine)?;
            }
        }
        Ok(())
    }

    fn run_graph(self, engine: &Engine) -> Result<(), SQLError> {
        match self {
            Self::Graph => {
                assert!(engine
                    .graph_with("g", |store| store.get_vertex(1))
                    .map_err(graph_error)?
                    .unwrap()
                    .map_err(graph_error)?
                    .is_some());
            }
            Self::Cypher => {
                assert_eq!(
                    engine
                        .run_cypher("g", "MATCH (n:P) RETURN n", BTreeMap::new())
                        .map_err(graph_error)?
                        .1
                        .len(),
                    1
                );
            }
            Self::GraphExists => {
                assert!(engine.has_graph("g").map_err(graph_error)?);
            }
            Self::GraphNames => {
                assert_eq!(engine.list_graphs().map_err(graph_error)?, vec!["g"]);
            }
            Self::GraphLabels => {
                assert!(engine
                    .list_graph_labels("g")
                    .map_err(graph_error)?
                    .is_some());
            }
            Self::GraphCatalog => {
                assert_eq!(engine.graph_label_catalog().map_err(graph_error)?.len(), 1);
            }
            Self::Path => {
                assert!(engine
                    .get_path_index("p", "g")
                    .map_err(graph_error)?
                    .is_some());
            }
            Self::PathNames => {
                assert_eq!(
                    engine.list_path_indexes().map_err(graph_error)?,
                    vec!["g::p"]
                );
            }
            _ => unreachable!("only graph query variants enter this helper"),
        }
        Ok(())
    }
}

fn calibrated_vectors(engine: &Engine, provider: usize, table: &str) -> Result<usize, SQLError> {
    let target = VectorCalibrationTarget {
        corpus_id: "public.docs".into(),
        corpus_version: "docs-v1".into(),
        index_id: "public.docs.embedding".into(),
        index_version: "embedding-v1".into(),
        index_kind: if provider == 0 {
            "sqlite-bruteforce"
        } else {
            "keyvalue-bruteforce"
        }
        .into(),
        embedding_model_id: "fixture".into(),
        embedding_model_version: "1".into(),
        candidate_k: 5,
        dimensions: 2,
    };
    let model = VectorCalibrationModel::new(
        VectorProbabilityTransform::new(0.05, 0.9, 0.3, 0.2).unwrap(),
        VectorCalibrationProvenance {
            model_version: "fixture-v1".into(),
            target: target.clone(),
            fit_sample_count: 500,
        },
    )
    .unwrap();
    engine
        .calibrated_vector_search_with_model(table, "embedding", [1.0, 0.0], &model, &target)
        .map(|entries| entries.len())
}

fn hybrid_search(engine: &Engine, table: &str, robust: bool) -> Result<usize, SQLError> {
    if robust {
        engine.robust_hybrid_search(&RobustHybridSearchParams {
            table,
            text_field: "body",
            text_query: "indexed",
            vector_field: "embedding",
            query_vector: vec![1.0, 0.0],
            knn_pool: 5,
            top_k: 5,
            alpha: 0.5,
        })
    } else {
        engine.hybrid_search(&HybridSearchParams {
            table,
            text_field: "body",
            text_query: "indexed",
            vector_field: "embedding",
            query_vector: vec![1.0, 0.0],
            knn_pool: 5,
            top_k: 5,
        })
    }
    .map(|entries| entries.len())
}

#[test]
fn first_direct_queries_select_one_snapshot_before_execution_and_keep_it_after_savepoint_undo() {
    let (_directory, sessions) = fixtures();
    for (provider, setup) in sessions.into_iter().enumerate() {
        prepare(&setup.engine);
        for query in Query::ALL {
            let a = setup.sibling();
            a.sql("BEGIN ISOLATION LEVEL SERIALIZABLE; SAVEPOINT before_query");
            assert!(participant(&a).is_none(), "{query:?}");
            setup.sql("UPDATE left_t SET v = 2");
            query
                .run(&a.engine, provider, "docs")
                .unwrap_or_else(|error| panic!("{query:?}, provider={provider}: {error}"));
            let original = participant(&a)
                .unwrap_or_else(|| panic!("{query:?} did not admit its participant"));
            setup.sql("UPDATE left_t SET v = 3");
            a.sql("ROLLBACK TO before_query");
            assert_eq!(
                a.sql("SELECT v FROM left_t").rows[0]["v"],
                Value::Int(2),
                "{query:?}"
            );
            query.run(&a.engine, provider, "docs").unwrap();
            assert_eq!(participant(&a), Some(original), "{query:?}");
            assert_eq!(
                a.engine
                    .sql("SET TRANSACTION ISOLATION LEVEL READ COMMITTED", &[])
                    .unwrap_err()
                    .sqlstate(),
                Some("25001")
            );
            a.sql("ROLLBACK");
            assert_eq!(a.sql("SELECT v FROM left_t").rows[0]["v"], Value::Int(3));
        }
    }
}

#[test]
fn direct_query_validation_and_execution_errors_abort_the_active_savepoint() {
    let (_directory, sessions) = fixtures();
    for (provider, a) in sessions.into_iter().enumerate() {
        prepare(&a.engine);
        for query in [
            Query::Text,
            Query::Profiled,
            Query::Parameters,
            Query::Calibration,
            Query::Knn,
            Query::VectorSimilarity,
            Query::VectorModel,
            Query::Hybrid,
            Query::RobustHybrid,
            Query::Physical,
        ] {
            a.sql(
                "BEGIN; UPDATE right_t SET v = 2; SAVEPOINT before_query; UPDATE right_t SET v = 3",
            );
            assert!(
                query.run(&a.engine, provider, "missing_table").is_err(),
                "{query:?}"
            );
            assert!(a.engine.transaction_failed(), "{query:?}");
            assert_eq!(
                a.engine.sql("SELECT 1", &[]).unwrap_err().sqlstate(),
                Some("25P02")
            );
            a.sql("ROLLBACK TO before_query");
            assert_eq!(a.sql("SELECT v FROM right_t").rows[0]["v"], Value::Int(2));
            query.run(&a.engine, provider, "docs").unwrap();
            a.sql("COMMIT");
        }
        for query in ["BROKEN CYPHER", "RETURN unknown_function(1)"] {
            a.sql(
                "BEGIN; UPDATE right_t SET v = 2; SAVEPOINT before_query; UPDATE right_t SET v = 3",
            );
            a.engine
                .run_cypher("g", query, BTreeMap::new())
                .unwrap_err();
            assert!(a.engine.transaction_failed(), "{query}");
            a.sql("ROLLBACK TO before_query");
            assert_eq!(a.sql("SELECT v FROM right_t").rows[0]["v"], Value::Int(2));
            Query::Cypher.run(&a.engine, provider, "docs").unwrap();
            a.sql("COMMIT");
        }
    }
}

fn default_deferrable_query(cancel: bool) {
    for provider in 0..3 {
        let directory = tempfile::tempdir().unwrap();
        let (writer, records) = observed_fixture(provider, &directory.path().join("queries.redb"));
        prepare(&writer.engine);
        for query in [
            Query::Profiled,
            Query::Parameters,
            Query::Hybrid,
            Query::Physical,
            Query::Graph,
            Query::Cypher,
        ] {
            let reader = writer.sibling();
            reader.sql("SET default_transaction_isolation = 'serializable'; SET default_transaction_read_only = on; SET default_transaction_deferrable = on");
            writer.sql("BEGIN ISOLATION LEVEL SERIALIZABLE; SELECT v FROM left_t");
            let previous = participant(&writer).unwrap();
            let candidate = SerializableTransactionId::new(
                previous.database(),
                previous.coordinator(),
                previous.allocation() + 1,
            )
            .unwrap();
            let cancellation = reader.engine.cancellation_token();
            std::thread::scope(|scope| {
                let read = scope.spawn(|| query.run(&reader.engine, provider, "docs"));
                pending_snapshot(records.as_ref(), candidate, &cancellation);
                if cancel {
                    cancellation.cancel();
                    assert_eq!(
                        read.join().unwrap().unwrap_err().sqlstate(),
                        Some("57014"),
                        "{query:?}"
                    );
                } else {
                    let commit = writer
                        .engine
                        .sql("UPDATE right_t SET v = v + 1; COMMIT", &[]);
                    if commit.is_err() {
                        cancellation.cancel();
                    }
                    commit.unwrap();
                    read.join().unwrap().unwrap();
                }
            });
            assert_eq!(reader.engine.transaction_depth(), 0, "{query:?}");
            assert!(participant(&reader).is_none(), "{query:?}");
            if cancel {
                cancellation.reset();
                writer.sql("COMMIT");
            }
            query.run(&reader.engine, provider, "docs").unwrap();
            assert_eq!(reader.engine.transaction_depth(), 0);
        }
    }
}

#[test]
fn implicit_direct_queries_use_default_deferrable_admission_and_complete() {
    default_deferrable_query(false);
}

#[test]
fn cancelled_direct_query_admission_releases_its_frame_and_keeps_typed_diagnostics() {
    default_deferrable_query(true);
}

#[test]
fn a_failed_cypher_read_rolls_back_its_implicitly_created_graph() {
    let (_directory, sessions) = fixtures();
    for engine in std::iter::once(Engine::new()).chain(sessions.into_iter().map(|s| s.engine)) {
        engine
            .run_cypher("new_graph", "RETURN unknown_function(1)", BTreeMap::new())
            .unwrap_err();
        assert_eq!(engine.transaction_depth(), 0);
        assert!(!engine.has_graph("new_graph").unwrap());
    }
}

#[test]
fn first_direct_retrieval_and_graph_reads_participate_in_write_skew_detection() {
    for query in [
        Query::Text,
        Query::Profiled,
        Query::Knn,
        Query::VectorModel,
        Query::Hybrid,
        Query::Physical,
        Query::Graph,
        Query::Cypher,
    ] {
        let (_directory, sessions) = fixtures();
        for (provider, a) in sessions.into_iter().enumerate() {
            prepare(&a.engine);
            let b = a.sibling();
            a.sql("BEGIN ISOLATION LEVEL SERIALIZABLE");
            b.sql("BEGIN ISOLATION LEVEL SERIALIZABLE; SELECT v FROM right_t");
            query.run(&a.engine, provider, "docs").unwrap();
            a.sql("UPDATE right_t SET v = 2");
            if matches!(query, Query::Graph | Query::Cypher) {
                let mut vertex = Vertex::new(1, "P");
                vertex
                    .properties
                    .insert("changed".into(), Value::Bool(true));
                b.engine.add_graph_vertex(vertex, "g").unwrap();
            } else {
                b.sql("UPDATE docs SET body = 'replacement', embedding = ARRAY[0.0, 1.0]");
            }
            a.sql("COMMIT");
            assert_eq!(
                b.engine.sql("COMMIT", &[]).unwrap_err().sqlstate(),
                Some("40001"),
                "{query:?}, provider={provider}"
            );
            assert_eq!(b.engine.transaction_depth(), 0);
        }
    }
}

#[test]
fn direct_calibration_keeps_its_signal_name_and_obeys_read_only_defaults() {
    let (_directory, sessions) = fixtures();
    for (provider, engine) in sessions
        .into_iter()
        .map(|s| s.engine)
        .chain(std::iter::once(Engine::new()))
        .enumerate()
    {
        prepare(&engine);
        for query in [
            Query::Parameters,
            Query::Calibration,
            Query::Hybrid,
            Query::RobustHybrid,
        ] {
            engine.drop_scoring_params("docs.body").unwrap();
            engine
                .sql("SET default_transaction_read_only = on", &[])
                .unwrap();
            query.run(&engine, provider, "docs").unwrap();
            assert_eq!(engine.transaction_depth(), 0);
            assert!(engine.load_scoring_params("docs.body").unwrap().is_none());
            engine
                .sql("SET default_transaction_read_only = off", &[])
                .unwrap();
            query.run(&engine, provider, "docs").unwrap();
            assert_eq!(engine.transaction_depth(), 0);
            assert!(engine.load_scoring_params("docs.body").unwrap().is_some());
            assert!(engine
                .load_scoring_params("public.docs.body")
                .unwrap()
                .is_none());
        }
    }
}

#[test]
fn read_only_calibration_preserves_stale_parameters_and_rejects_explicit_publication() {
    let (_directory, sessions) = fixtures();
    for engine in sessions
        .into_iter()
        .map(|s| s.engine)
        .chain(std::iter::once(Engine::new()))
    {
        prepare(&engine);
        let stale = r#"{"alpha":77.0,"beta":11.0,"estimated_doc_count":16.0}"#;
        engine.save_scoring_params("docs.body", stale).unwrap();
        engine.sql("BEGIN READ ONLY", &[]).unwrap();
        let params = engine.bayesian_params_for("docs", "body").unwrap();
        assert_ne!(params.alpha, 77.0);
        assert_eq!(
            engine
                .sql("SELECT id FROM docs WHERE fts_match(body, 'indexed')", &[])
                .unwrap()
                .rows
                .len(),
            1
        );
        assert_eq!(
            engine.load_scoring_params("docs.body").unwrap().as_deref(),
            Some(stale)
        );
        let error = engine.save_scoring_params("docs.body", "{}").unwrap_err();
        assert_eq!(error.sqlstate(), Some("25006"));
        engine.rollback().unwrap();
        assert_eq!(
            engine.load_scoring_params("docs.body").unwrap().as_deref(),
            Some(stale)
        );
        engine.bayesian_params_for("docs", "body").unwrap();
        assert_ne!(
            engine.load_scoring_params("docs.body").unwrap().as_deref(),
            Some(stale)
        );
    }
}

#[test]
fn read_only_calibration_finishes_while_an_independent_parameter_writer_remains_open() {
    let (_directory, sessions) = fixtures();
    for session in sessions {
        let writer = session.engine;
        prepare(&writer);
        writer.drop_scoring_params("docs.body").unwrap();
        let reader = writer.new_session().unwrap();
        writer.begin().unwrap();
        writer
            .save_scoring_params("docs.body", r#"{"alpha":77.0,"beta":11.0}"#)
            .unwrap();
        let (finished_tx, finished_rx) = std::sync::mpsc::channel();
        let task = std::thread::spawn(move || {
            let result = (|| {
                reader.sql("BEGIN ISOLATION LEVEL SERIALIZABLE READ ONLY", &[])?;
                let params = reader.bayesian_params_for("docs", "body")?;
                reader.commit()?;
                Ok::<_, SQLError>(params)
            })();
            finished_tx.send(result).unwrap();
        });
        let result = finished_rx.recv_timeout(std::time::Duration::from_secs(10));
        writer.rollback().unwrap();
        task.join().unwrap();
        let params = result
            .expect("read-only calibration waited for the parameter writer")
            .unwrap();
        assert_ne!(params.alpha, 77.0);
        assert!(writer.load_scoring_params("docs.body").unwrap().is_none());
    }
}
