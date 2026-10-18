//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use uqa_engine::Engine;

fn open(provider: usize, path: &std::path::Path) -> Engine {
    match provider {
        0 => Engine::new(),
        1 => Engine::open(path).unwrap(),
        2 => Engine::from_persistent_provider(std::sync::Arc::new(
            uqa_storage_sqlite::SQLiteKeyValueStorage::open(path).unwrap(),
        ))
        .unwrap(),
        3 => Engine::from_persistent_provider(std::sync::Arc::new(
            uqa_storage_redb::RedbStorage::open(path).unwrap(),
        ))
        .unwrap(),
        _ => unreachable!(),
    }
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn from_routine_planning_matches_postgresql_and_reopen(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("from-routine-planning.db");
    let engine = open(provider, &path);
    let mut reference: serde_json::Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/parity/pg18/from_routine_planning_oracle.expected.json"
    )))
    .unwrap();
    crate::pg18_oracle::verify(&engine, &reference.to_string());
    if provider == 0 {
        return;
    }
    drop(engine);
    let engine = open(provider, &path);
    engine
        .sql("SET search_path TO routine_from, public", &[])
        .unwrap();
    reference["cases"].as_array_mut().unwrap().retain(|case| {
        matches!(
            case["id"].as_str().unwrap(),
            "domain_unfetched"
                | "assignment_distinct"
                | "assignment_distinct_on"
                | "assignment_ordered"
                | "assignment_grouped"
                | "assignment_grouping_sets"
                | "assignment_lateral"
                | "domain_rows"
                | "correlated"
                | "correlated_implicit"
                | "correlated_collision"
                | "cte_rows"
                | "cte_correlated_rows"
                | "unused_argument"
                | "conditional_argument"
                | "collated_arg"
                | "shadowed_caller_cte"
                | "missing_not_caller_cte"
                | "single_rows_from"
                | "multi_rows_from"
                | "ordinality_unfetched"
                | "record_unfetched"
                | "record_wrong_width"
                | "scalar_unfetched"
                | "procedural_unfetched"
                | "nested_sql"
                | "nested_procedural"
                | "procedural_width"
                | "catch_error"
                | "catch_reused"
                | "rethrow_error"
                | "rethrow_reused"
                | "handler_error"
                | "constant_expression"
                | "assignment_error"
                | "constant_assignment"
                | "query_error"
                | "perform_error"
        )
    });
    assert_eq!(reference["cases"].as_array().unwrap().len(), 38);
    crate::pg18_oracle::verify(&engine, &reference.to_string());
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn set_returning_contexts_match_postgresql_and_reopen(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("set-returning-contexts.db");
    let engine = open(provider, &path);
    let mut reference: serde_json::Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/parity/pg18/set_returning_contexts_oracle.expected.json"
    )))
    .unwrap();
    crate::pg18_oracle::verify(&engine, &reference.to_string());
    if provider == 0 {
        return;
    }
    drop(engine);
    // The rows each expanded INSERT stored stay as written, stored views keep expanding their set-returning calls in window keys and deferring the functions in FROM, a stored rule still evaluates a set-returning VALUES entry outside a target list, and stored routines keep their placement and column definition list diagnostics and their FROM column names.
    let engine = open(provider, &path);
    reference["cases"].as_array_mut().unwrap().retain(|case| {
        matches!(
            case["id"].as_str().unwrap(),
            "update_set_user"
                | "call_argument"
                | "call_argument_user"
                | "call_argument_valid"
                | "fixtures_unchanged"
                | "window_view_rows"
                | "insert_regexp_rows"
                | "insert_m_rows"
                | "insert_k_rows"
                | "insert_n_rows"
                | "insert_partitioned_rows"
                | "insert_rule_reads_set"
                | "insert_rule_rows"
                | "coldef_user_scalar"
                | "coldef_record_required"
                | "coldef_record"
                | "coldef_out_parameters"
                | "coldef_composite"
                | "coldef_composite_out"
                | "coldef_composite_domain"
                | "lazy_view_unfetched"
                | "lazy_view_fetched"
                | "lazy_user_rows_from"
                | "lazy_user_view_unfetched"
                | "lazy_user_view_fetched"
                | "user_alias_reference"
                | "user_rows_from_columns"
        )
    });
    assert_eq!(reference["cases"].as_array().unwrap().len(), 27);
    crate::pg18_oracle::verify(&engine, &reference.to_string());
}
