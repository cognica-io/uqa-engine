//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_scoring::{
    VectorCalibrationModel, VectorCalibrationProvenance, VectorCalibrationTarget,
    VectorProbabilityTransform,
};

fn create(engine: &Engine) {
    sql(engine, "CREATE TABLE diskann_docs(id int, embedding vector(2)); INSERT INTO diskann_docs VALUES(1,ARRAY[1.0,0.0]),(2,ARRAY[0.0,1.0]),(3,ARRAY[-1.0,0.0]); CREATE INDEX diskann_idx ON diskann_docs USING diskann(embedding)");
}

fn target(engine: &Engine) -> VectorCalibrationTarget {
    engine
        .diskann_calibration_target("diskann_docs", "embedding", "fixture", "1", 3)
        .unwrap()
}

fn model(target: &VectorCalibrationTarget) -> VectorCalibrationModel {
    VectorCalibrationModel::new(
        VectorProbabilityTransform::new(0.0, 1.0, 1.0, 0.5).unwrap(),
        VectorCalibrationProvenance {
            model_version: "fixed-fixture".into(),
            target: target.clone(),
            fit_sample_count: 100,
        },
    )
    .unwrap()
}

fn search(
    engine: &Engine,
    target: &VectorCalibrationTarget,
) -> Result<Vec<uqa_core::ScoredEntry>, uqa_sql::SQLError> {
    engine.calibrated_vector_search_with_model(
        "diskann_docs",
        "embedding",
        [1.0, 0.0],
        &model(target),
        target,
    )
}

fn probabilities(engine: &Engine, target: &VectorCalibrationTarget) {
    let actual = search(engine, target).unwrap();
    assert_eq!(actual.len(), 3);
    for (entry, expected) in actual.iter().zip([1, 2, 3]) {
        let row = engine
            .get_document("diskann_docs", entry.doc_id)
            .unwrap()
            .unwrap();
        assert_eq!(row.get("id"), Some(&Value::Int(expected)));
    }
    // Independent sigmoid values for log likelihood ratios 0.5, -0.5 and -1.5 with one neutral prior.
    for (actual, expected) in actual.iter().zip([
        0.622_459_331_201_854_6,
        0.377_540_668_798_145_4,
        0.182_425_523_806_356_35,
    ]) {
        assert!((actual.score - expected).abs() < 1e-12, "{actual:?}");
    }
}

fn changes(engine: &Engine) {
    create(engine);
    let original = target(engine);
    engine
        .save_vector_calibration_model("diskann-fixed", &model(&original))
        .unwrap();
    assert_eq!(
        target(engine),
        original,
        "model catalog writes must not change vector identity"
    );
    probabilities(engine, &original);
    let mut forged = original.clone();
    forged.corpus_version = "caller-controlled".into();
    assert!(search(engine, &forged)
        .unwrap_err()
        .to_string()
        .contains("target mismatch"));
    sql(
        engine,
        "BEGIN; SAVEPOINT kept; UPDATE diskann_docs SET embedding=ARRAY[-1.0,0.0] WHERE id=1",
    );
    let private = target(engine);
    assert_ne!(private.corpus_version, original.corpus_version);
    assert_eq!(private.index_version, original.index_version);
    assert_eq!(search(engine, &private).unwrap().len(), 3);
    assert!(search(engine, &original).is_err());
    sql(engine, "ROLLBACK TO kept");
    assert_eq!(target(engine), original);
    probabilities(engine, &original);
    sql(engine, "ROLLBACK");
    let snapshot = engine.capture_statement_read_snapshot().unwrap();
    let reader = engine.statement_read_snapshot_engine(&snapshot);
    sql(engine, "DROP INDEX diskann_idx; CREATE INDEX diskann_idx ON diskann_docs USING diskann(embedding) WITH(max_degree=2,search_list_size=2,beam_width=1)");
    assert_ne!(target(engine).index_version, original.index_version);
    assert!(search(engine, &original).is_err());
    probabilities(&reader, &original);
}

#[test]
fn diskann_calibration_rejects_stale_targets_and_preserves_retained_probabilities() {
    changes(&Engine::new());
    for provider in 0..3 {
        let (_directory, engine, peer) = sessions(provider);
        drop(peer);
        changes(&engine);
    }
}

#[test]
fn diskann_calibration_private_marker_does_not_hide_new_committed_vectors() {
    for provider in 0..3 {
        let (_directory, engine, peer) = sessions(provider);
        create(&engine);
        sql(
            &engine,
            "BEGIN; UPDATE diskann_docs SET embedding=ARRAY[-1.0,0.0] WHERE id=1",
        );
        let before = target(&engine);
        sql(
            &peer,
            "UPDATE diskann_docs SET embedding=ARRAY[1.0,0.0] WHERE id=2",
        );
        let after = target(&engine);
        assert_ne!(
            before.corpus_version, after.corpus_version,
            "provider {provider}"
        );
        assert_eq!(before.index_version, after.index_version);
        assert_eq!(search(&engine, &after).unwrap().len(), 3);
        assert!(search(&engine, &before).is_err());
        sql(&engine, "ROLLBACK");
    }
}

#[test]
fn diskann_calibration_persisted_target_survives_reopen_and_unrelated_commits() {
    for provider in 0..3 {
        let (_directory, engine, peer) = sessions(provider);
        create(&engine);
        let original = target(&engine);
        engine
            .save_vector_calibration_model("diskann-fixed", &model(&original))
            .unwrap();
        sql(
            &peer,
            "CREATE TABLE unrelated(id int); INSERT INTO unrelated VALUES(1)",
        );
        assert_eq!(target(&engine), original);
        assert_eq!(target(&peer), original);
        let factory = Arc::clone(engine.storage.provider.as_ref().unwrap());
        drop((engine, peer));
        let reopened = Engine::from_persistent_provider(factory).unwrap();
        assert_eq!(target(&reopened), original);
        let saved = reopened
            .load_vector_calibration_model("diskann-fixed")
            .unwrap()
            .unwrap();
        assert_eq!(saved, model(&original));
        probabilities(&reopened, &original);
    }
}

#[test]
fn diskann_calibration_copied_target_follows_fixed_rows_and_the_new_definition() {
    for provider in 0..3 {
        let (_directory, engine, peer) = sessions(provider);
        sql(&engine, "CREATE TABLE diskann_docs(id int, embedding vector(2)); INSERT INTO diskann_docs VALUES(1,ARRAY[1.0,0.0]),(2,ARRAY[0.0,1.0]),(3,ARRAY[-1.0,0.0])");
        sql(
            &engine,
            "BEGIN ISOLATION LEVEL REPEATABLE READ; SELECT * FROM diskann_docs",
        );
        sql(&peer, "UPDATE diskann_docs SET embedding=ARRAY[-1.0,0.0] WHERE id=1; DELETE FROM diskann_docs WHERE id=3; INSERT INTO diskann_docs VALUES(4,ARRAY[1.0,0.0])");
        sql(
            &engine,
            "CREATE INDEX diskann_idx ON diskann_docs USING diskann(embedding)",
        );
        let table = engine.try_table("diskann_docs").unwrap().unwrap();
        let live = table
            .vector_indexes
            .read()
            .get("embedding")
            .unwrap()
            .diskann_query_metadata(&engine.query_retention_control().unwrap())
            .unwrap()
            .unwrap();
        let snapshot = engine.capture_statement_read_snapshot().unwrap();
        let reader = engine.statement_read_snapshot_engine(&snapshot);
        let fixed = target(&reader);
        let selected = reader
            .require_query_table("diskann_docs")
            .unwrap()
            .vector_indexes
            .read()
            .get("embedding")
            .unwrap()
            .diskann_query_metadata(&reader.query_retention_control().unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(live.manifest, selected.manifest);
        assert_ne!(live.corpus_fingerprint, selected.corpus_fingerprint);
        probabilities(&reader, &fixed);
        sql(&engine, "ROLLBACK");
        let nested = reader.capture_statement_read_snapshot().unwrap();
        let nested = reader.statement_read_snapshot_engine(&nested);
        assert_eq!(target(&nested), fixed);
        drop(reader);
        probabilities(&nested, &fixed);
    }
}
