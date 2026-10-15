//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Independent `PostgreSQL` constraint nodes, catalog reconstruction and transaction observations.

use uqa_engine::Engine;

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn constraint_expression_catalogs_match_postgresql(#[case] provider: usize) {
    verify(provider, include_str!("constraint_expressions.json"));
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn foreign_key_delete_columns_match_postgresql(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("constraint-delete-columns.db");
    let transcript = include_str!("constraint_delete_columns.json");
    {
        let engine = open(provider, &path);
        verify_reference(&engine, transcript);
    }
    if provider != 0 {
        let mut reference: serde_json::Value = serde_json::from_str(transcript).unwrap();
        let last = reference["cases"]
            .as_array()
            .unwrap()
            .last()
            .unwrap()
            .clone();
        reference["cases"] = serde_json::json!([last]);
        verify_reference(&open(provider, &path), &reference.to_string());
    }
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn defaults_indexes_and_domain_collations_match_postgresql(#[case] provider: usize) {
    verify(provider, include_str!("constraint_expression_edges.json"));
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn selected_constraint_calls_match_postgresql(#[case] provider: usize) {
    verify(provider, include_str!("constraint_routine_calls.json"));
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn constraint_relation_context_matches_postgresql(#[case] provider: usize) {
    verify(provider, include_str!("constraint_expression_context.json"));
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn declaration_visibility_matches_postgresql(#[case] provider: usize) {
    verify(
        provider,
        include_str!("constraint_declaration_visibility.json"),
    );
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn catalog_node_inputs_match_postgresql(#[case] provider: usize) {
    verify(provider, include_str!("catalog_node_inputs.json"));
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn partition_node_catalogs_match_postgresql(#[case] provider: usize) {
    verify(provider, include_str!("partition_node_catalogs.json"));
}

#[rstest::rstest]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn declaration_visibility_survives_reopen(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("constraint-reopen.db");
    {
        let engine = open(provider, &path);
        verify_reference(
            &engine,
            include_str!("constraint_declaration_visibility.json"),
        );
    }
    let engine = open(provider, &path);
    verify_reference(&engine, include_str!("constraint_declaration_reopen.json"));
}

#[rstest::rstest]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn constraint_catalog_publication_matches_postgresql_sessions(#[case] provider: usize) {
    verify_sessions(
        provider,
        &serde_json::from_str(include_str!("constraint_peers.json")).unwrap(),
    );
}

#[rstest::rstest]
#[case::sqlite_add(1, "add")]
#[case::sqlite_rename(1, "rename")]
#[case::sqlite_drop(1, "drop")]
#[case::sqlite_key_value_add(2, "add")]
#[case::sqlite_key_value_rename(2, "rename")]
#[case::sqlite_key_value_drop(2, "drop")]
#[case::redb_add(3, "add")]
#[case::redb_rename(3, "rename")]
#[case::redb_drop(3, "drop")]
#[case::sqlite_equal_payload(1, "equal_payload")]
#[case::sqlite_unrelated(1, "unrelated")]
#[case::sqlite_key_value_equal_payload(2, "equal_payload")]
#[case::sqlite_key_value_unrelated(2, "unrelated")]
#[case::redb_equal_payload(3, "equal_payload")]
#[case::redb_unrelated(3, "unrelated")]
fn catalog_private_row_versions_match_postgresql(#[case] provider: usize, #[case] history: &str) {
    let histories: serde_json::Value =
        serde_json::from_str(include_str!("catalog_private_versions.json")).unwrap();
    verify_sessions(provider, &histories[history]);
}

#[rstest::rstest]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn catalog_serializable_row_versions_match_postgresql(
    #[case] provider: usize,
    #[values("add", "rename", "drop", "equal_payload", "unrelated")] history: &str,
) {
    let histories: serde_json::Value =
        serde_json::from_str(include_str!("catalog_serializable_versions.json")).unwrap();
    verify_sessions(provider, &histories[history]);
}

#[rstest::rstest]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn derived_catalog_row_versions_match_postgresql(
    #[case] provider: usize,
    #[values("views", "domain_validation", "foreign", "trigger", "portal")] history: &str,
) {
    let histories: serde_json::Value =
        serde_json::from_str(include_str!("catalog_derived_visibility.json")).unwrap();
    verify_sessions(provider, &histories[history]);
}

fn verify_sessions(provider: usize, reference: &serde_json::Value) {
    let directory = tempfile::tempdir().unwrap();
    let writer = open(provider, &directory.path().join("constraint-peers.db"));
    let reader = writer.new_session().unwrap();
    for case in reference["cases"].as_array().unwrap() {
        let engine = match case["session"].as_u64().unwrap() {
            0 => &writer,
            1 => &reader,
            _ => unreachable!(),
        };
        let transcript = serde_json::json!({
            "postgresql_version": reference["postgresql_version"],
            "cases": [case],
        });
        verify_reference(engine, &transcript.to_string());
    }
}

fn verify(provider: usize, transcript: &str) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("constraint-expressions.db");
    verify_reference(&open(provider, &path), transcript);
}

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

fn verify_reference(engine: &Engine, transcript: &str) {
    let mut oracle: serde_json::Value = serde_json::from_str(transcript).unwrap();
    // PostgreSQL prints native C char bytes with platform-specific signedness; preserve every node field and physical byte.
    for case in oracle["cases"].as_array_mut().unwrap() {
        for result in case["results"].as_array_mut().into_iter().flatten() {
            let columns = result["columns"]
                .as_array()
                .unwrap()
                .iter()
                .enumerate()
                .filter_map(|(index, column)| {
                    matches!(
                        column.as_str(),
                        Some(
                            "adbin"
                                | "indexprs"
                                | "indpred"
                                | "conbin"
                                | "proargdefaults"
                                | "retained_node"
                                | "relpartbound"
                        )
                    )
                    .then_some(index)
                })
                .collect::<Vec<_>>();
            for row in result["rows"].as_array_mut().unwrap() {
                for &column in &columns {
                    if let Some(value) = row[column].as_str() {
                        row[column] = uqa_sql::catalog::node_tree::parse(value)
                            .unwrap()
                            .to_string()
                            .into();
                    }
                }
            }
        }
    }
    crate::pg18_oracle::verify(engine, &serde_json::to_string(&oracle).unwrap());
}

#[path = "constraint_expressions/catalog_upgrade.rs"]
mod catalog_upgrade;

#[rstest::rstest]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn partition_foreign_keys_keep_independent_physical_rows(#[case] provider: usize) {
    let mut reference: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../tests/parity/pg18/create_table_order_oracle.expected.json"
    ))
    .unwrap();
    reference["cases"].as_array_mut().unwrap().retain(|case| {
        let sql = case["sql"].as_str().unwrap();
        sql.starts_with("CREATE TABLE cto_fr ") || sql.starts_with("CREATE TABLE cto_fq ")
            || sql.starts_with("CREATE TABLE cto_fq3 ")
            || sql == "SELECT conname, contype FROM pg_constraint WHERE conrelid = 'cto_fq3'::regclass ORDER BY 1;"
    });
    assert_eq!(reference["cases"].as_array().unwrap().len(), 4);
    verify(provider, &reference.to_string());
}
