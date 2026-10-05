//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Server declaration diagnostics and role dependencies against independent `PostgreSQL` results.

use super::declarations::open;

const ORACLE: &str =
    include_str!("../../../../tests/parity/pg18/foreign_server_identity_oracle.expected.json");

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn foreign_server_identity_matches_postgresql(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("foreign-servers.db");
    let engine = open(provider, &path);
    engine
        .sql("CREATE ROLE fs414_owner; CREATE ROLE fs414_other", &[])
        .unwrap();
    let mut transcript: serde_json::Value = serde_json::from_str(ORACLE).unwrap();
    transcript["cases"]
        .as_array_mut()
        .unwrap()
        .retain(|case| case["reference_only"] != true);
    crate::pg18_oracle::verify(&engine, &transcript.to_string());
    if provider > 0 {
        drop(engine);
        let reopened = open(provider, &path);
        for case in transcript["cases"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|case| {
                matches!(
                    case["id"].as_str(),
                    Some(
                        "renamed_owner_dependency"
                            | "other_owner_dependency"
                            | "public_owner_dependency_projection"
                    )
                )
            })
        {
            let sql = case["sql"].as_str().unwrap();
            let actual = crate::pg18_oracle::run_case(&reopened, sql);
            assert_eq!(actual["error"], case["error"], "reopened: {sql}");
            assert_eq!(actual["results"], case["results"], "reopened: {sql}");
        }
    }
}
