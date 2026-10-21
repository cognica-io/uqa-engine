//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Public robust text pooling uses the established native scoring and retained provider state.

use super::{copy_engine, exec, native_signals, scores, Engine, Interleave};
use std::collections::{BTreeMap, BTreeSet};
use uqa_core::Value;
use uqa_fusion::{LogitGating, RobustPositiveEvidencePool};

fn statement(all_fields: bool, configured: bool) -> String {
    let field = if all_fields { "'_all'" } else { "body" };
    let options = if configured {
        ",alpha=>0.8,gating=>'pass',weights=>ARRAY[0.2,0.3,0.5],
        logit_min=>ARRAY[-2.0,-3.0,-4.0],logit_max=>ARRAY[2.0,3.0,4.0]"
    } else {
        ""
    };
    format!(
        "COPY (SELECT id,_score FROM copy_fusion WHERE pool_positive_evidence(
        bayesian_match({field},'alpha'),bayesian_match({field},'absent'),
        bayesian_match({field},'beta'){options})) TO STDOUT"
    )
}

fn expected(engine: &Engine, all_fields: bool, configured: bool) -> BTreeMap<i64, u64> {
    let inputs = native_signals(engine, all_fields);
    let prior = uqa_scoring::sigmoid(
        [inputs.prior; 3]
            .iter()
            .map(|prior| uqa_scoring::logit(*prior))
            .sum::<f64>()
            / 3.0,
    );
    let fuser = RobustPositiveEvidencePool::new(if configured { 0.8 } else { 0.5 })
        .unwrap()
        .with_logit_gating(if configured {
            LogitGating::Pass
        } else {
            LogitGating::Softplus
        })
        .with_base_rate(prior)
        .unwrap();
    inputs
        .signals
        .iter()
        .flat_map(|map| map.keys().copied())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(|id| {
            let probabilities = inputs
                .signals
                .iter()
                .map(|map| map.get(&id).copied())
                .collect::<Vec<_>>();
            let score = fuser
                .fuse_configured(
                    &probabilities,
                    configured.then_some(&[0.2, 0.3, 0.5]),
                    configured.then_some(&[-2.0, -3.0, -4.0]),
                    configured.then_some(&[2.0, 3.0, 4.0]),
                )
                .unwrap();
            (inputs.identities[&id], score.to_bits())
        })
        .collect()
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn copy_positive_pool_keeps_native_bits_options_and_statement_generation(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("pool-copy.db");
    let engine = copy_engine(provider, &path);
    exec(&engine, "CREATE TABLE copy_fusion(id INTEGER PRIMARY KEY,body TEXT,title TEXT);
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
        for configured in [false, true] {
            let expected = expected(&engine, all_fields, configured);
            let mut bytes = Vec::new();
            assert_eq!(
                engine
                    .copy_to(&statement(all_fields, configured), &mut bytes)
                    .unwrap(),
                513
            );
            assert_eq!(scores(bytes), expected);
        }
    }
    exec(
        &engine,
        "BEGIN; UPDATE copy_fusion SET body='absent',title=NULL WHERE id=513",
    );
    let mut bytes = Vec::new();
    assert_eq!(
        engine.copy_to(&statement(true, true), &mut bytes).unwrap(),
        513
    );
    assert_eq!(scores(bytes), expected(&engine, true, true));
    exec(
        &engine,
        "UPDATE copy_fusion SET body='changed' WHERE id=513",
    );
    let mut bytes = Vec::new();
    assert_eq!(
        engine.copy_to(&statement(true, true), &mut bytes).unwrap(),
        512
    );
    assert_eq!(scores(bytes), expected(&engine, true, true));
    exec(&engine, "ROLLBACK");
    let before = expected(&engine, true, true);
    let writer = (provider != 0).then(|| engine.new_session().unwrap());
    let mut output = Interleave {
        writer: writer.as_ref(),
        bytes: Vec::new(),
    };
    assert_eq!(
        engine.copy_to(&statement(true, true), &mut output).unwrap(),
        513
    );
    assert_eq!(scores(output.bytes), before);
    let after = expected(&engine, true, true);
    assert_eq!(after.len(), if provider == 0 { 513 } else { 512 });
    drop(writer);
    drop(engine);
    if provider != 0 {
        let reopened = copy_engine(provider, &path);
        let mut bytes = Vec::new();
        assert_eq!(
            reopened
                .copy_to(&statement(true, true), &mut bytes)
                .unwrap(),
            512
        );
        assert_eq!(scores(bytes), after);
    }
}

#[test]
fn copy_positive_pool_failures_preserve_explicit_transaction_rollback() {
    struct Sink(Option<uqa_core::CancellationToken>);
    impl std::io::Write for Sink {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            match &self.0 {
                Some(token) => {
                    token.cancel();
                    Ok(bytes.len())
                }
                None => Err(std::io::Error::other("positive pool output closed")),
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
            .copy_to(&statement(false, true), &mut sink)
            .unwrap_err();
        if cancel {
            assert_eq!(error.sqlstate(), Some("57014"));
        } else {
            assert!(error.to_string().contains("positive pool output closed"));
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
