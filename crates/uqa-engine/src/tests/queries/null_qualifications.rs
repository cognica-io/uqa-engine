//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::tests::providers::Provider;
use uqa_core::Value;

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn nullable_qualifications_preserve_postgresql_effects(#[case] provider: Provider) {
    let reference: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../../tests/parity/pg18/null_qualifications.expected.json"
    ))
    .unwrap();
    assert!(reference["version"]
        .as_str()
        .unwrap()
        .starts_with("PostgreSQL 18."));
    for case in reference["cases"].as_array().unwrap() {
        let directory = tempfile::tempdir().unwrap();
        let engine = provider.open(&directory.path().join("null-qualifications.db"));
        let name = case["name"].as_str().unwrap();
        for key in ["setup", "operation"] {
            engine
                .sql(case[key].as_str().unwrap(), &[])
                .unwrap_or_else(|error| panic!("{name} {key}: {error}"));
        }
        let result = engine
            .sql(
                &format!("SELECT ({})::text", case["observe"].as_str().unwrap()),
                &[],
            )
            .unwrap();
        let Some(Value::Str(observed)) = result.value_at(0, 0) else {
            panic!("{name}: JSON observation");
        };
        let observed: serde_json::Value = serde_json::from_str(observed).unwrap();
        assert_eq!(observed, case["expected"], "{name}");
    }
}
