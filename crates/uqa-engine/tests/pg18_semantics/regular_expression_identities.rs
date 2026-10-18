//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use uqa_engine::Engine;

/// The cases whose results describe the final state: catalog identities, stored CHECK constraints, defaults, generated columns, indexes, domains, views and routine bodies, and the EXECUTE grants and revocations in force at the end of the transcript.
const RETAINED: &[&str] = &[
    "catalog",
    "catalog_arguments",
    "catalog_signatures",
    "regprocedure_count",
    "regprocedure_instr",
    "regprocedure_like_match",
    "regprocedure_replace",
    "regprocedure_substr",
    "regprocedure_output",
    "check_like_rejects",
    "check_count_rejects",
    "update_check_rejects",
    "generated_rows",
    "check_definitions",
    "default_definitions",
    "index_definition",
    "index_rows",
    "partial_index_definition",
    "domain_accepts",
    "domain_rejects",
    "domain_definition",
    "view_rows",
    "view_definition",
    "named_view_rows",
    "named_view_definition",
    "set_view_rows",
    "select_set_view_rows",
    "sql_function_calls",
    "sql_function_body",
    "plpgsql_call",
    "sql_function_pieces_call",
    "sql_function_kind_call",
    "routines_types",
    "routines_user_types",
    "routines_user_definitions",
    "acl_grant",
    "acl_granted_select_list",
    "acl_granted_from",
    "acl_granted_still_revoked",
    "acl_granted_routines_visible",
    "acl_granted_view",
    "acl_granted_select_set_view",
    "acl_granted_reset",
    "acl_granted_observation",
    "acl_granted_catalog",
];

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

fn reference() -> serde_json::Value {
    serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/parity/pg18/regular_expression_identities_oracle.expected.json"
    )))
    .unwrap()
}

fn retained(mut reference: serde_json::Value) -> String {
    reference["cases"]
        .as_array_mut()
        .unwrap()
        .retain(|case| RETAINED.contains(&case["id"].as_str().unwrap()));
    assert_eq!(reference["cases"].as_array().unwrap().len(), RETAINED.len());
    reference.to_string()
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn regular_expression_identities_match_postgresql_and_reopen(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("regular-expression-identities.db");
    let engine = open(provider, &path);
    crate::pg18_oracle::verify(&engine, &reference().to_string());
    if provider == 0 {
        return;
    }
    drop(engine);
    let engine = open(provider, &path);
    crate::pg18_oracle::verify(&engine, &retained(reference()));
}

/// Definitions stored before these functions had catalog signatures retain unbound calls. After reopening they select the same declarations, evaluate, print and authorize as definitions stored with bindings.
#[test]
fn definitions_stored_without_regular_expression_bindings_load_and_execute() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory
        .path()
        .join("regular-expression-legacy-bindings.db");
    {
        let engine = Engine::open(&path).unwrap();
        crate::pg18_oracle::verify(&engine, &reference().to_string());
    }
    assert!(remove_regular_expression_bindings(&path) >= 20);
    let engine = Engine::open(&path).unwrap();
    crate::pg18_oracle::verify(&engine, &retained(reference()));
}

fn remove_regular_expression_bindings(path: &std::path::Path) -> usize {
    let catalog =
        crate::native_storage::catalog(uqa_storage_sqlite::ManagedConnection::open(path).unwrap())
            .unwrap();
    let mut removed = 0;
    for mut table in catalog.load_tables().unwrap() {
        let mut columns: serde_json::Value = serde_json::from_str(&table.columns_json).unwrap();
        let mut constraints: serde_json::Value =
            serde_json::from_str(&table.constraints_json).unwrap();
        let count = unbind(&mut columns) + unbind(&mut constraints);
        if count > 0 {
            table.columns_json = columns.to_string();
            table.constraints_json = constraints.to_string();
            catalog.save_table(&table).unwrap();
            removed += count;
        }
    }
    for mut view in catalog.load_views().unwrap() {
        let mut definition: serde_json::Value =
            serde_json::from_str(&view.definition_json).unwrap();
        let count = unbind(&mut definition);
        if count > 0 {
            view.definition_json = definition.to_string();
            catalog.save_view(&view).unwrap();
            removed += count;
        }
    }
    for mut index in catalog.load_catalog_indexes().unwrap() {
        let Some(encoded) = &index.definition_json else {
            continue;
        };
        let mut definition: serde_json::Value = serde_json::from_str(encoded).unwrap();
        let count = unbind(&mut definition);
        if count > 0 {
            index.definition_json = Some(definition.to_string());
            catalog.save_catalog_index_row(&index).unwrap();
            removed += count;
        }
    }
    for key in ["sql_functions_json", "sql_domains_json"] {
        let encoded = catalog.get_metadata(key).unwrap().unwrap();
        let mut metadata: serde_json::Value = serde_json::from_str(&encoded).unwrap();
        let count = unbind(&mut metadata);
        if count > 0 {
            catalog.set_metadata(key, &metadata.to_string()).unwrap();
            removed += count;
        }
    }
    removed
}

/// Remove the bindings of regular-expression calls, the form earlier versions stored.
fn unbind(value: &mut serde_json::Value) -> usize {
    match value {
        serde_json::Value::Object(fields) => {
            let mut removed = fields
                .get_mut("Func")
                .and_then(serde_json::Value::as_object_mut)
                .map_or(0, |function| {
                    let regular_expression = function
                        .get("binding")
                        .and_then(|binding| binding["name"].as_str())
                        .is_some_and(|name| name.starts_with("pg_catalog.regexp_"));
                    usize::from(regular_expression && function.remove("binding").is_some())
                });
            for value in fields.values_mut() {
                removed += unbind(value);
            }
            removed
        }
        serde_json::Value::Array(values) => values.iter_mut().map(unbind).sum(),
        _ => 0,
    }
}
