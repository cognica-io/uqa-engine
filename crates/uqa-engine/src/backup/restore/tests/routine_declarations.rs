//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::tests::providers::Provider;

mod corruption;
mod preserved;

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn routine_declaration_diagnostics_match_postgresql_with_eager_and_lazy_bodies(
    #[case] provider: Provider,
) {
    let reference: serde_json::Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../uqa-sql/src/routines/declaration/captured/tests/pg18.json"
    )))
    .unwrap();
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("declarations"));
    engine
        .sql(
            "CREATE DOMAIN declaration_int AS integer; CREATE DOMAIN declaration_ints AS integer[]",
            &[],
        )
        .unwrap();
    let cases = reference["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 44);
    for case in cases {
        engine.sql("BEGIN", &[]).unwrap();
        engine
            .sql(
                &format!(
                    "SET LOCAL check_function_bodies = {}",
                    case["mode"].as_str().unwrap()
                ),
                &[],
            )
            .unwrap();
        let result = engine.sql(case["sql"].as_str().unwrap(), &[]);
        let actual = match result {
            Ok(_) => serde_json::json!({"sqlstate": null, "message": null}),
            Err(error) => {
                serde_json::json!({"sqlstate": error.sqlstate(), "message": error.to_string()})
            }
        };
        assert_eq!(
            actual,
            serde_json::json!({"sqlstate": case["sqlstate"], "message": case["message"]}),
            "{provider:?}: {} {}",
            case["mode"],
            case["id"]
        );
        assert_eq!(engine.transaction_failed(), !case["sqlstate"].is_null());
        engine.sql("ROLLBACK", &[]).unwrap();
        assert!(!engine.transaction_failed());
        assert_eq!(engine.transaction_depth(), 0);
        assert!(!engine
            .durable
            .sql_user_functions
            .read()
            .contains_key("public.f"));
    }
}
