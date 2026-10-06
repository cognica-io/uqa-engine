//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` ordered-set call resolution, input effects and stored syntax.

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
fn within_group_calls_match_postgresql(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("within-group-calls.db");
    let engine = open(provider, &path);
    let transcript =
        include_str!("../../../../tests/parity/pg18/within_group_calls_oracle.expected.json");
    crate::pg18_oracle::verify(&engine, transcript);
    if provider == 0 {
        return;
    }
    drop(engine);
    let engine = open(provider, &path);
    let mut restored: serde_json::Value = serde_json::from_str(transcript).unwrap();
    restored["cases"].as_array_mut().unwrap().retain(|case| {
        matches!(
            case["id"].as_str().unwrap(),
            "ordinary_ordered_aggregates"
                | "mode"
                | "percentile"
                | "filtered_percentile"
                | "view_result"
                | "view_deparse"
        )
    });
    assert_eq!(restored["cases"].as_array().unwrap().len(), 6);
    crate::pg18_oracle::verify(&engine, &restored.to_string());
}

#[test]
fn legacy_stored_ordered_set_syntax_survives_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("legacy-ordered-set.db");
    let mut fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../tests/parity/pg18/within_group_calls_oracle.expected.json"
    ))
    .unwrap();
    fixture["cases"].as_array_mut().unwrap().retain(|case| {
        matches!(
            case["id"].as_str().unwrap(),
            "setup" | "view_definition" | "view_result" | "view_deparse"
        )
    });
    let engine = open(1, &path);
    crate::pg18_oracle::verify(&engine, &fixture.to_string());
    drop(engine);
    {
        let catalog = crate::native_storage::catalog(
            uqa_storage_sqlite::ManagedConnection::open(&path).unwrap(),
        )
        .unwrap();
        let mut rows = catalog.load_views().unwrap();
        let row = rows
            .iter_mut()
            .find(|row| row.relation.name == "saved")
            .unwrap();
        let mut definition: serde_json::Value = serde_json::from_str(&row.definition_json).unwrap();
        assert!(remove_written_order_syntax(&mut definition) > 0);
        row.definition_json = definition.to_string();
        catalog.save_view(row).unwrap();
    }
    fixture["cases"]
        .as_array_mut()
        .unwrap()
        .retain(|case| matches!(case["id"].as_str().unwrap(), "view_result" | "view_deparse"));
    for _ in 0..2 {
        let engine = open(1, &path);
        crate::pg18_oracle::verify(&engine, &fixture.to_string());
    }
}

fn remove_written_order_syntax(value: &mut serde_json::Value) -> usize {
    match value {
        serde_json::Value::Object(fields) => {
            let removed = usize::from(fields.remove("order_syntax").is_some());
            if removed > 0 && fields.get("name").and_then(serde_json::Value::as_str) == Some("mode")
            {
                fields.insert("binding".into(), serde_json::Value::Null);
            }
            removed
                + fields
                    .values_mut()
                    .map(remove_written_order_syntax)
                    .sum::<usize>()
        }
        serde_json::Value::Array(values) => {
            values.iter_mut().map(remove_written_order_syntax).sum()
        }
        _ => 0,
    }
}
