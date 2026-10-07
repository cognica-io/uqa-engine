//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Builtin routine authority follows the selected catalog identity and execution lifetime.

use std::{path::Path, sync::Arc};
use uqa_engine::Engine;

fn open(provider: usize, path: &Path) -> Engine {
    match provider {
        0 => Engine::new(),
        1 => Engine::open(path).unwrap(),
        2 => Engine::from_persistent_provider(Arc::new(
            uqa_storage_sqlite::SQLiteKeyValueStorage::open(path).unwrap(),
        ))
        .unwrap(),
        3 => Engine::from_persistent_provider(Arc::new(
            uqa_storage_redb::RedbStorage::open(path).unwrap(),
        ))
        .unwrap(),
        _ => unreachable!(),
    }
}

fn reference(source: &str, keep: impl Fn(&serde_json::Value) -> bool) -> String {
    let mut value: serde_json::Value = serde_json::from_str(source).unwrap();
    value["cases"].as_array_mut().unwrap().retain(keep);
    assert_ne!(value["cases"].as_array().unwrap().len(), 0);
    value.to_string()
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn builtin_privileges_match_postgresql(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("builtin-acl.db");
    let engine = open(provider, &path);
    crate::pg18_oracle::verify(
        &engine,
        include_str!("../../../../../tests/parity/pg18/builtin_routine_acl_oracle.expected.json"),
    );
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn builtin_initialization_matches_postgresql(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("builtin-init.db");
    let engine = open(provider, &path);
    crate::pg18_oracle::verify(
        &engine,
        include_str!(
            "../../../../../tests/parity/pg18/builtin_routine_initialization_oracle.expected.json"
        ),
    );
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn builtin_catalog_acl_lifecycle_matches_postgresql(#[case] provider: usize) {
    const SOURCE: &str = include_str!(
        "../../../../../tests/parity/pg18/builtin_routine_lifecycle_oracle.expected.json"
    );
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("builtin-lifecycle.db");
    let engine = open(provider, &path);
    crate::pg18_oracle::verify(&engine, &reference(SOURCE, |case| case["id"] != "cleanup"));
    let engine = if provider == 0 {
        engine
    } else {
        drop(engine);
        let engine = open(provider, &path);
        crate::pg18_oracle::verify(
            &engine,
            &reference(SOURCE, |case| {
                case["id"].as_str().unwrap().starts_with("reopen_")
            }),
        );
        engine
    };
    crate::pg18_oracle::verify(&engine, &reference(SOURCE, |case| case["id"] == "cleanup"));
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn builtin_expression_liveness_matches_postgresql(#[case] provider: usize) {
    const SOURCE: &str = include_str!(
        "../../../../../tests/parity/pg18/builtin_routine_liveness_oracle.expected.json"
    );
    let directory = tempfile::tempdir().unwrap();
    let engine = open(provider, &directory.path().join("builtin-live.db"));

    crate::pg18_oracle::verify(
        &engine,
        &reference(SOURCE, |case| {
            case["id"] != "explain_table_dead_case" && case["id"] != "cleanup"
        }),
    );
    let source: serde_json::Value = serde_json::from_str(SOURCE).unwrap();
    let explain = source["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["id"] == "explain_table_dead_case")
        .unwrap();
    let actual = crate::pg18_oracle::run_case(&engine, explain["sql"].as_str().unwrap());
    // The authority test compares EXPLAIN completion, not the engine's physical plan text.
    assert_eq!(actual["error"], explain["error"]);
    assert_eq!(actual["command_tags"], explain["command_tags"]);
    crate::pg18_oracle::verify(&engine, &reference(SOURCE, |case| case["id"] == "cleanup"));
}

#[test]
fn sum_and_window_catalog_declarations_match_postgresql() {
    crate::pg18_oracle::verify(
        &Engine::new(),
        include_str!(
            "../../../../../tests/parity/pg18/builtin_routine_catalog_oracle.expected.json"
        ),
    );
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn discarded_values_preserve_analyzed_column_authority(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let engine = open(
        provider,
        &directory.path().join("builtin-column-authority.db"),
    );
    crate::pg18_oracle::verify(&engine, include_str!("../../../../../tests/parity/pg18/builtin_routine_column_authority_oracle.expected.json"));
}

#[rstest::rstest]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn prepared_folded_and_surviving_calls_follow_peer_acl_changes(
    #[case] provider: usize,
    #[values(0, 1)] fixture: usize,
) {
    let directory = tempfile::tempdir().unwrap();
    let engine = open(provider, &directory.path().join("builtin-peer-acl.db"));
    let peer = engine.new_session().unwrap();
    let reference = [
        include_str!(
            "../../../../../tests/parity/pg18/builtin_routine_peer_acl_oracle.expected.json"
        ),
        include_str!(
            "../../../../../tests/parity/pg18/builtin_routine_peer_relation_oracle.expected.json"
        ),
    ][fixture];
    let oracle: serde_json::Value = serde_json::from_str(reference).unwrap();
    for case in oracle["cases"].as_array().unwrap() {
        let session = if case["session"] == "a" {
            &engine
        } else {
            &peer
        };
        crate::pg18_oracle::verify(
            session,
            &serde_json::json!({
                "postgresql_version": oracle["postgresql_version"],
                "cases": [case],
            })
            .to_string(),
        );
    }
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn mutation_initialization_matches_postgresql(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let engine = open(provider, &directory.path().join("builtin-mutation.db"));
    crate::pg18_oracle::verify(
        &engine,
        include_str!(
            "../../../../../tests/parity/pg18/builtin_routine_mutations_oracle.expected.json"
        ),
    );
}
