//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Definitions an earlier release stored without input collations derive them on the first open, so their CHECK constraints, defaults, views, rules, trigger conditions and domain constraints keep `PostgreSQL`'s selections, and `ON CONFLICT` still infers an expression index stored without them.

use serde_json::Value as Json;
use std::sync::Arc;
use uqa_engine::Engine;
use uqa_storage::{CatalogFacade, PersistentStorageProvider};

/// The transcript cases that create the definitions (`setup`) or probe them after the upgrade.
fn verify(engine: &Engine, setup: bool) {
    let mut reference: Json = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/parity/pg18/collation_case_mapping_oracle.expected.json"
    )))
    .unwrap();
    reference["cases"].as_array_mut().unwrap().retain(|case| {
        let id = case["id"].as_str().unwrap();
        id.starts_with("up_") && id.starts_with("up_create_") == setup
    });
    assert_eq!(
        reference["cases"].as_array().unwrap().len(),
        if setup { 10 } else { 12 }
    );
    crate::pg18_oracle::verify(engine, &reference.to_string());
}

/// Remove every recorded input collation, and the binding of each `regexp_like` call, which an earlier release stored unbound. Returns how many selections were removed.
fn predecessor(value: &mut Json) -> usize {
    match value {
        Json::Object(object) => {
            let mut removed = usize::from(object.remove("input_collation").is_some());
            if let Some(Json::Object(call)) = object.get_mut("Func") {
                if call
                    .get("name")
                    .and_then(Json::as_str)
                    .is_some_and(|name| name.ends_with("regexp_like"))
                {
                    call.insert("binding".into(), Json::Null);
                }
            }
            removed += object.values_mut().map(predecessor).sum::<usize>();
            removed
        }
        Json::Array(items) => items.iter_mut().map(predecessor).sum(),
        _ => 0,
    }
}

fn predecessor_json(source: &str, count: &mut usize) -> String {
    let mut value: Json = serde_json::from_str(source).unwrap();
    *count += predecessor(&mut value);
    value.to_string()
}

fn downgrade(catalog: &dyn CatalogFacade) {
    let mut count = 0;
    for mut row in catalog.load_tables().unwrap() {
        row.columns_json = predecessor_json(&row.columns_json, &mut count);
        row.constraints_json = predecessor_json(&row.constraints_json, &mut count);
        catalog.save_table(&row).unwrap();
    }
    for mut row in catalog.load_views().unwrap() {
        row.definition_json = predecessor_json(&row.definition_json, &mut count);
        catalog.save_view(&row).unwrap();
    }
    for key in ["sql_functions_json", "sql_rules_json", "sql_triggers_json"] {
        let source = catalog.get_metadata(key).unwrap().unwrap();
        catalog
            .set_metadata(key, &predecessor_json(&source, &mut count))
            .unwrap();
    }
    // Format 3 is the rule catalog of the release before conditions recorded input collations.
    let mut rules: Json =
        serde_json::from_str(&catalog.get_metadata("sql_rules_json").unwrap().unwrap()).unwrap();
    rules["format_version"] = 3.into();
    catalog
        .set_metadata("sql_rules_json", &rules.to_string())
        .unwrap();
    for (key, source) in catalog.metadata_with_prefix("uqa.sql.domain.v1:").unwrap() {
        catalog
            .set_metadata(&key, &predecessor_json(&source, &mut count))
            .unwrap();
    }
    for mut row in catalog.load_catalog_indexes().unwrap() {
        row.columns_json = predecessor_json(&row.columns_json, &mut count);
        row.definition_json = row
            .definition_json
            .as_deref()
            .map(|source| predecessor_json(source, &mut count));
        catalog.save_catalog_index_row(&row).unwrap();
    }
    assert!(count >= 10, "only {count} recorded selections removed");
}

/// The stored definitions that derive and persist their selections on the first open. Index keys keep the ones they were built with, and rules derive theirs whenever they are loaded.
fn derived_definitions(catalog: &dyn CatalogFacade) -> Vec<String> {
    let mut definitions = Vec::new();
    for row in catalog.load_tables().unwrap() {
        definitions.push(row.columns_json);
        definitions.push(row.constraints_json);
    }
    for row in catalog.load_views().unwrap() {
        definitions.push(row.definition_json);
    }
    definitions.push(catalog.get_metadata("sql_triggers_json").unwrap().unwrap());
    definitions.extend(
        catalog
            .metadata_with_prefix("uqa.sql.domain.v1:")
            .unwrap()
            .into_iter()
            .map(|(_, source)| source),
    );
    definitions.sort();
    definitions
}

fn provider(provider: usize, path: &std::path::Path) -> Arc<dyn PersistentStorageProvider> {
    match provider {
        1 => Arc::new(uqa_storage_sqlite::SQLiteStorageProvider::new(
            uqa_storage_sqlite::ManagedConnection::open(path).unwrap(),
        )),
        2 => Arc::new(uqa_storage_sqlite::SQLiteKeyValueStorage::open(path).unwrap()),
        3 => Arc::new(uqa_storage_redb::RedbStorage::open(path).unwrap()),
        _ => unreachable!(),
    }
}

#[rstest::rstest]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn definitions_stored_without_input_collations_derive_them_once(#[case] storage: usize) {
    let directory = tempfile::tempdir().unwrap();
    let factory = provider(storage, &directory.path().join("collation-predecessor.db"));
    let engine = Engine::from_persistent_provider(Arc::clone(&factory)).unwrap();
    verify(&engine, true);
    {
        let session = factory.open_session().unwrap();
        session.backend.begin_transaction().unwrap();
        downgrade(session.catalog.as_ref());
        session.backend.commit_transaction().unwrap();
    }
    // A secondary session refuses a catalog that still needs the derivation.
    let error = engine
        .new_session()
        .err()
        .expect("a peer must not migrate the durable catalog");
    assert!(error.to_string().contains("initial-open"), "{error}");
    drop(engine);
    let engine = Engine::from_persistent_provider(Arc::clone(&factory)).unwrap();
    verify(&engine, false);
    drop(engine);
    let session = factory.open_session().unwrap();
    let derived = derived_definitions(session.catalog.as_ref());
    assert!(
        derived
            .iter()
            .all(|source| !lacks_selection(&serde_json::from_str(source).unwrap())),
        "a stored definition kept a call without its input collation"
    );
    drop(session);
    let engine = Engine::from_persistent_provider(Arc::clone(&factory)).unwrap();
    drop(engine);
    let session = factory.open_session().unwrap();
    assert_eq!(
        derived,
        derived_definitions(session.catalog.as_ref()),
        "a second open rewrote the derived definitions"
    );
}

/// Whether a serialized definition still has a case-mapping, ILIKE or regular-expression call without a recorded selection.
fn lacks_selection(value: &Json) -> bool {
    match value {
        Json::Object(object) => {
            if let Some(Json::Object(call)) = object.get("Func") {
                let binding = call.get("binding").and_then(|binding| {
                    serde_json::from_value::<uqa_sql::ast::FunctionBinding>(binding.clone()).ok()
                });
                if uqa_sql::binding::legacy_collations::call_lacks_input_collation(
                    call.get("name").and_then(Json::as_str).unwrap_or_default(),
                    binding.as_ref(),
                ) {
                    return true;
                }
            }
            object.values().any(lacks_selection)
        }
        Json::Array(items) => items.iter().any(lacks_selection),
        _ => false,
    }
}
