//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Finite vector pools preserve native fusion observations inside bounded COPY.

use super::{copy_engine, exec, scores, Engine};
use std::collections::BTreeMap;
use uqa_core::Value;
use uqa_engine::operator_tree_bridge::EngineDriver;
use uqa_execution::operator_tree::{OperatorOutput, OperatorTreeDriver};
use uqa_operators::{GatingSpec, OperatorTree, TextScoringMode};

fn tree(k: usize, mode: usize) -> OperatorTree {
    let vector = if mode == 3 {
        OperatorTree::CalibratedVectorMatch {
            query_vector: vec![1.0, 0.0],
            k,
            field: "embedding".into(),
            threshold: Some(0.6),
        }
    } else {
        OperatorTree::CosineProbability(Box::new(OperatorTree::KNN {
            query_vector: vec![1.0, 0.0],
            k,
            field: "embedding".into(),
        }))
    };
    let signals = vec![
        OperatorTree::Term {
            query: "alpha".into(),
            field: Some("body".into()),
            scoring: Some(TextScoringMode::BayesianBM25),
            top_k: None,
        },
        vector,
    ];
    if mode == 2 {
        OperatorTree::RobustPositiveEvidencePool {
            signals,
            alpha: 0.5,
            gating: GatingSpec::Softplus,
            weights: None,
            logit_min: None,
            logit_max: None,
            adaptive_weights: false,
        }
    } else {
        OperatorTree::BayesianEvidenceFusion {
            signals,
            base_rate: None,
        }
    }
}

fn statement(k: usize, mode: usize) -> String {
    let vector = if mode == 3 {
        format!("calibrated_vector_match(embedding,ARRAY[1.0,0.0],{k},0.6)")
    } else {
        format!("knn_match(embedding,ARRAY[1.0,0.0],{k})")
    };
    let predicate = match mode {
        0 => format!("text_match(body,'alpha') AND {vector}"),
        2 => format!("pool_positive_evidence(bayesian_match(body,'alpha'),{vector})"),
        _ => format!("fuse_bayesian_evidence(bayesian_match(body,'alpha'),{vector})"),
    };
    format!("COPY (SELECT id,_score FROM copy_vector WHERE {predicate}) TO STDOUT")
}

fn expected(engine: &Engine, k: usize, mode: usize) -> BTreeMap<i64, u64> {
    let identities = engine
        .sql("SELECT id,_doc_id FROM copy_vector", &[])
        .unwrap()
        .rows
        .into_iter()
        .map(|row| match (&row["id"], &row["_doc_id"]) {
            (Value::Int(id), Value::Int(doc)) => (u64::try_from(*doc).unwrap(), *id),
            other => panic!("integer identities: {other:?}"),
        })
        .collect::<BTreeMap<_, _>>();
    // Direct physical execution retains the existing materializing fusion route.
    let OperatorOutput::Posting(postings) = EngineDriver::new(engine, "copy_vector", &[])
        .execute_node(&tree(k, mode))
        .unwrap()
    else {
        panic!("fusion returns postings");
    };
    postings
        .into_iter()
        .map(|entry| (identities[&entry.doc_id], entry.payload.score.to_bits()))
        .collect()
}

fn verify(engine: &Engine, k: usize, mode: usize) {
    if k == 0 {
        let mut bytes = Vec::new();
        let error = engine.copy_to(&statement(k, mode), &mut bytes).unwrap_err();
        assert!(error.to_string().contains("k must be positive"), "{error}");
        assert!(bytes.is_empty());
        return;
    }
    let expected = expected(engine, k, mode);
    let mut bytes = Vec::new();
    assert_eq!(
        engine.copy_to(&statement(k, mode), &mut bytes).unwrap() as usize,
        expected.len()
    );
    let actual = scores(bytes);
    let differences = actual
        .keys()
        .chain(expected.keys())
        .filter(|id| actual.get(id) != expected.get(id))
        .map(|id| (*id, actual.get(id).copied(), expected.get(id).copied()))
        .collect::<Vec<_>>();
    assert!(
        differences.is_empty(),
        "mode={mode}, k={k}, differences={differences:?}"
    );
}

struct Interleave<'a> {
    writer: Option<&'a Engine>,
    bytes: Vec<u8>,
}
impl std::io::Write for Interleave<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if let Some(writer) = self.writer.take() {
            exec(
                writer,
                "UPDATE copy_vector SET body='changed',embedding=ARRAY[-1.0,0.0] WHERE id=128;
                DELETE FROM copy_vector WHERE id=129;
                INSERT INTO copy_vector VALUES(130,'alpha',ARRAY[1.0,0.0])",
            );
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn copy_vector_fusion_preserves_native_bits_and_selected_generation(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("vector-fusion-copy.db");
    let engine = copy_engine(provider, &path);
    exec(
        &engine,
        "CREATE TABLE copy_vector(id INTEGER PRIMARY KEY,body TEXT,embedding VECTOR(2));
        INSERT INTO copy_vector SELECT n,CASE WHEN n%3=0 THEN 'beta' ELSE 'alpha alpha' END,
        ARRAY[1.0,(n-64)::DOUBLE PRECISION/64] FROM generate_series(1,129) AS s(n);
        CREATE INDEX copy_vector_body ON copy_vector USING gin(body);
        SET work_mem='64kB'",
    );
    engine
        .save_scoring_params(
            "copy_vector.body",
            "{\"alpha\":1.2,\"beta\":0.7,\"base_rate\":0.2}",
        )
        .unwrap();
    for mode in 0..4 {
        for k in [0, 1, 8] {
            verify(&engine, k, mode);
        }
    }
    exec(
        &engine,
        "BEGIN; UPDATE copy_vector SET body='alpha',embedding=ARRAY[1.0,0.0] WHERE id=3;
        UPDATE copy_vector SET body='beta',embedding=ARRAY[-1.0,0.0] WHERE id=2",
    );
    for mode in 0..4 {
        verify(&engine, 8, mode);
    }
    exec(&engine, "ROLLBACK");

    let before = expected(&engine, 8, 0);
    let writer = (provider != 0).then(|| engine.new_session().unwrap());
    let mut sink = Interleave {
        writer: writer.as_ref(),
        bytes: Vec::new(),
    };
    engine.copy_to(&statement(8, 0), &mut sink).unwrap();
    assert_eq!(scores(sink.bytes), before);
    for mode in 0..4 {
        verify(&engine, 8, mode);
    }
    drop(writer);
    drop(engine);
    if provider != 0 {
        let reopened = copy_engine(provider, &path);
        for mode in 0..4 {
            verify(&reopened, 8, mode);
        }
    }
}

#[test]
fn copy_vector_fusion_output_failures_preserve_rollback() {
    struct Sink(Option<uqa_core::CancellationToken>);
    impl std::io::Write for Sink {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if let Some(token) = &self.0 {
                token.cancel();
                Ok(bytes.len())
            } else {
                Err(std::io::Error::other("vector fusion output closed"))
            }
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let engine = Engine::new();
    exec(&engine, "CREATE TABLE copy_vector(id INTEGER PRIMARY KEY,body TEXT,embedding VECTOR(2));
        INSERT INTO copy_vector SELECT n,'alpha',ARRAY[1.0,n::DOUBLE PRECISION] FROM generate_series(1,513) AS s(n);
        CREATE INDEX copy_vector_body ON copy_vector USING gin(body); SET work_mem='64kB'");
    for cancel in [false, true] {
        exec(
            &engine,
            "BEGIN; UPDATE copy_vector SET body='beta' WHERE id=1",
        );
        let mut sink = Sink(cancel.then(|| engine.cancellation_token()));
        let error = engine.copy_to(&statement(8, 0), &mut sink).unwrap_err();
        if cancel {
            assert_eq!(error.sqlstate(), Some("57014"));
        } else {
            assert!(error.to_string().contains("vector fusion output closed"));
        }
        engine.reset_cancellation();
        exec(&engine, "ROLLBACK");
        assert_eq!(
            engine
                .sql("SELECT body FROM copy_vector WHERE id=1", &[])
                .unwrap()
                .rows[0]["body"],
            Value::Str("alpha".into())
        );
    }
}
