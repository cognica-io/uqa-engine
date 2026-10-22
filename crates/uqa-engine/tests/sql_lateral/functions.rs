//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Independent and correlated FROM evaluation across native providers.

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn lateral_reference_positions_match_postgresql(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("lateral-positions.db");
    let engine = super::values::open(provider, &path);
    crate::pg18_oracle::verify(
        &engine,
        include_str!("../../../../tests/parity/pg18/lateral_positions_oracle.expected.json"),
    );
    if provider != 0 {
        drop(engine);
        let reopened = super::values::open(provider, &path);
        let reference: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../tests/parity/pg18/lateral_positions_oracle.expected.json"
        ))
        .unwrap();
        let retained = reference["cases"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|case| case["id"] == "runtime_position")
            .collect::<Vec<_>>();
        assert_eq!(retained.len(), 1);
        let replay = serde_json::json!({"postgresql_version": reference["postgresql_version"], "cases": retained});
        crate::pg18_oracle::verify(&reopened, &replay.to_string());
    }
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn lateral_function_inputs_match_postgresql(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("lateral-functions.db");
    let engine = super::values::open(provider, &path);
    crate::pg18_oracle::verify(
        &engine,
        include_str!("../../../../tests/parity/pg18/lateral_functions_oracle.expected.json"),
    );
    if provider != 0 {
        drop(engine);
        let reopened = super::values::open(provider, &path);
        let reference: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../tests/parity/pg18/lateral_functions_oracle.expected.json"
        ))
        .unwrap();
        let retained = reference["cases"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|case| {
                case["id"].as_str().is_some_and(|id| {
                    id.starts_with("declared_user_pair")
                        || id.starts_with("declared_user_record")
                        || id.starts_with("wildcard_view_alias")
                })
            })
            .collect::<Vec<_>>();
        assert_eq!(retained.len(), 9);
        let replay = serde_json::json!({
            "postgresql_version": reference["postgresql_version"],
            "cases": retained,
        });
        crate::pg18_oracle::verify(&reopened, &replay.to_string());
    }
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn nested_lateral_namespaces_match_postgresql(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("lateral-namespaces.db");
    let engine = super::values::open(provider, &path);
    crate::pg18_oracle::verify(
        &engine,
        include_str!("../../../../tests/parity/pg18/lateral_namespace_oracle.expected.json"),
    );
    if provider != 0 {
        drop(engine);
        let reopened = super::values::open(provider, &path);
        let reference: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../tests/parity/pg18/lateral_namespace_oracle.expected.json"
        ))
        .unwrap();
        let cases = reference["cases"].as_array().unwrap();
        let retained = cases
            .iter()
            .filter(|case| {
                matches!(
                    case["id"].as_str(),
                    Some(
                        "nested_view_rows"
                            | "nested_view_definition"
                            | "nested_view_definition_pretty"
                            | "comma_view_definition"
                            | "comma_view_definition_pretty"
                            | "cross_view_definition"
                            | "cross_view_definition_pretty"
                            | "target_after_delete"
                            | "nested_named_view_rows"
                            | "nested_sql_function_rows"
                            | "nested_atomic_function_rows"
                            | "nested_star_view_rows"
                            | "nested_bare_star_view_rows"
                            | "namespace_rule_reset"
                            | "fire_namespace_rule"
                            | "namespace_rule_rows"
                    )
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(retained.len(), 16);
        let replay = serde_json::json!({
            "postgresql_version": reference["postgresql_version"],
            "cases": retained,
        });
        crate::pg18_oracle::verify(&reopened, &replay.to_string());
    }
}
