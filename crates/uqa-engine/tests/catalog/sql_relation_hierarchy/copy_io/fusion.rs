//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Public exact text-fusion capture, calibration, and retained statement state.

use super::{copy_engine, exec, Engine};
use std::collections::{BTreeMap, BTreeSet};
use uqa_core::Value;
use uqa_fusion::BayesianEvidenceFusion;
use uqa_scoring::{EvidenceLogit, ScoringMode};

#[path = "fusion/pooling.rs"]
mod pooling;
#[path = "fusion/vector.rs"]
mod vector;

struct NativeSignals {
    identities: BTreeMap<u64, i64>,
    signals: [BTreeMap<u64, f64>; 3],
    prior: f64,
}

fn native_signals(engine: &Engine, all_fields: bool) -> NativeSignals {
    let fields = if all_fields {
        &["body", "title"][..]
    } else {
        &["body"][..]
    };
    let identities = engine
        .sql("SELECT id,_doc_id FROM copy_fusion", &[])
        .unwrap();
    let identities = identities
        .rows
        .iter()
        .map(|row| {
            let (Value::Int(id), Value::Int(doc)) = (&row["id"], &row["_doc_id"]) else {
                panic!("integer identities");
            };
            (u64::try_from(*doc).unwrap(), *id)
        })
        .collect::<BTreeMap<_, _>>();
    let params = fields
        .iter()
        .map(|field| engine.bayesian_params_for("copy_fusion", field).unwrap())
        .collect::<Vec<_>>();
    let prior = {
        if all_fields {
            uqa_scoring::sigmoid(
                params
                    .iter()
                    .map(|p| uqa_scoring::logit(p.base_rate))
                    .sum::<f64>()
                    / params.len() as f64,
            )
        } else {
            params[0].base_rate
        }
    };
    let signals = ["alpha", "absent", "beta"].map(|query| {
        let mut scores = BTreeMap::<u64, f64>::new();
        for (field, params) in fields.iter().zip(&params) {
            for row in engine
                .search(
                    "copy_fusion",
                    field,
                    query,
                    &ScoringMode::BayesianBM25(params.evidence_params()),
                    usize::MAX,
                )
                .unwrap()
            {
                scores
                    .entry(row.doc_id)
                    .and_modify(|old| *old = old.max(row.score))
                    .or_insert(row.score);
            }
        }
        scores
    });
    NativeSignals {
        identities,
        signals,
        prior,
    }
}

fn native_scores(engine: &Engine, all_fields: bool, explicit: Option<f64>) -> BTreeMap<i64, u64> {
    let NativeSignals {
        identities,
        signals,
        prior,
    } = native_signals(engine, all_fields);
    let fusion = BayesianEvidenceFusion::new(explicit.unwrap_or(prior)).unwrap();
    signals
        .iter()
        .flat_map(|map| map.keys().copied())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(|id| {
            let evidence = signals
                .iter()
                .filter_map(|map| map.get(&id))
                .map(|p| EvidenceLogit::from_prior_free_probability(*p).unwrap())
                .collect::<Vec<_>>();
            (
                identities[&id],
                fusion.fuse(&evidence).unwrap().value().to_bits(),
            )
        })
        .collect()
}

fn copy_sql(all_fields: bool, explicit: bool) -> String {
    let field = if all_fields { "'_all'" } else { "body" };
    let prior = if explicit { ",base_rate=>0.1" } else { "" };
    format!(
        "COPY (SELECT id,_score FROM copy_fusion WHERE fuse_bayesian_evidence(
        bayesian_match({field},'alpha'),bayesian_match({field},'absent'),
        bayesian_match({field},'beta'){prior})) TO STDOUT"
    )
}

fn scores(bytes: Vec<u8>) -> BTreeMap<i64, u64> {
    String::from_utf8(bytes)
        .unwrap()
        .lines()
        .map(|line| {
            let (id, score) = line.split_once('\t').unwrap();
            (id.parse().unwrap(), score.parse::<f64>().unwrap().to_bits())
        })
        .collect()
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
                "UPDATE copy_fusion SET body='changed',title='changed' WHERE id=512;
                DELETE FROM copy_fusion WHERE id=513;
                INSERT INTO copy_fusion VALUES(514,'alpha','beta')",
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
fn copy_exact_text_fusion_preserves_native_bits_priors_and_generation(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("fusion-copy.db");
    let engine = copy_engine(provider, &path);
    exec(&engine, "CREATE TABLE copy_fusion(id INTEGER PRIMARY KEY, body TEXT, title TEXT);
        INSERT INTO copy_fusion SELECT n,
            CASE WHEN n%3=0 THEN 'alpha alpha beta' WHEN n%3=1 THEN 'alpha' ELSE 'beta' END,
            CASE WHEN n%2=0 THEN 'alpha beta beta' ELSE NULL END FROM generate_series(1,513) AS s(n);
        CREATE INDEX copy_fusion_body ON copy_fusion USING gin(body);
        CREATE INDEX copy_fusion_title ON copy_fusion USING gin(title);
        SET work_mem='64kB'");
    for (field, prior) in [("body", 0.2), ("title", 0.4)] {
        engine
            .save_scoring_params(
                &format!("copy_fusion.{field}"),
                &format!("{{\"alpha\":1.2,\"beta\":0.7,\"base_rate\":{prior}}}"),
            )
            .unwrap();
    }
    for all_fields in [false, true] {
        for explicit in [None, Some(0.1)] {
            let expected = native_scores(&engine, all_fields, explicit);
            let mut bytes = Vec::new();
            assert_eq!(
                engine
                    .copy_to(&copy_sql(all_fields, explicit.is_some()), &mut bytes)
                    .unwrap(),
                513
            );
            assert_eq!(scores(bytes), expected);
        }
    }
    let error = engine
        .copy_to(
            "COPY (SELECT id FROM copy_fusion WHERE fuse_bayesian_evidence(
        bayesian_match(body,'absent'),bayesian_match(title,'beta'))) TO STDOUT",
            &mut Vec::new(),
        )
        .unwrap_err();
    assert!(error.to_string().contains("one corpus prior"));
    exec(
        &engine,
        "BEGIN; UPDATE copy_fusion SET body='absent',title=NULL WHERE id=513",
    );
    let mut bytes = Vec::new();
    assert_eq!(
        engine.copy_to(&copy_sql(true, true), &mut bytes).unwrap(),
        513
    );
    assert_eq!(scores(bytes), native_scores(&engine, true, Some(0.1)));
    exec(
        &engine,
        "UPDATE copy_fusion SET body='changed' WHERE id=513",
    );
    let mut bytes = Vec::new();
    assert_eq!(
        engine.copy_to(&copy_sql(true, true), &mut bytes).unwrap(),
        512
    );
    assert_eq!(scores(bytes), native_scores(&engine, true, Some(0.1)));
    exec(&engine, "ROLLBACK");

    let expected = native_scores(&engine, true, Some(0.1));
    let writer = (provider != 0).then(|| engine.new_session().unwrap());
    let mut output = Interleave {
        writer: writer.as_ref(),
        bytes: Vec::new(),
    };
    assert_eq!(
        engine.copy_to(&copy_sql(true, true), &mut output).unwrap(),
        513
    );
    assert_eq!(scores(output.bytes), expected);
    let expected = native_scores(&engine, true, Some(0.1));
    assert_eq!(expected.len(), if provider == 0 { 513 } else { 512 });
    drop(writer);
    drop(engine);
    if provider != 0 {
        let reopened = copy_engine(provider, &path);
        let mut bytes = Vec::new();
        assert_eq!(
            reopened.copy_to(&copy_sql(true, true), &mut bytes).unwrap(),
            512
        );
        assert_eq!(scores(bytes), expected);
    }
}

#[test]
fn copy_exact_text_fusion_output_failures_keep_transaction_rollback() {
    struct Sink(Option<uqa_core::CancellationToken>);
    impl std::io::Write for Sink {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            match &self.0 {
                Some(token) => {
                    token.cancel();
                    Ok(bytes.len())
                }
                None => Err(std::io::Error::other("fusion output closed")),
            }
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE copy_fusion(id INTEGER PRIMARY KEY,body TEXT);
        INSERT INTO copy_fusion SELECT n,'alpha beta' FROM generate_series(1,513) AS s(n);
        CREATE INDEX copy_fusion_body ON copy_fusion USING gin(body);
        SET work_mem='64kB'",
    );
    for cancel in [false, true] {
        exec(
            &engine,
            "BEGIN; UPDATE copy_fusion SET body='alpha' WHERE id=1",
        );
        let mut sink = Sink(cancel.then(|| engine.cancellation_token()));
        let error = engine
            .copy_to(&copy_sql(false, true), &mut sink)
            .unwrap_err();
        if cancel {
            assert_eq!(error.sqlstate(), Some("57014"));
        } else {
            assert!(error.to_string().contains("fusion output closed"));
        }
        engine.reset_cancellation();
        exec(&engine, "ROLLBACK");
        assert_eq!(
            engine
                .sql("SELECT body FROM copy_fusion WHERE id=1", &[])
                .unwrap()
                .rows[0]["body"],
            Value::Str("alpha beta".into())
        );
    }
}
