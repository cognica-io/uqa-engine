//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retained provider definitions use the predecessor compiler's captured marker shapes.

use serde_json::{json, Value as Json};
use std::sync::Arc;
use uqa_engine::Engine;
use uqa_storage::{CatalogFacade, PersistentStorageProvider};

fn verify(engine: &Engine, setup: bool) {
    let mut reference: Json = serde_json::from_str(include_str!("restoration.json")).unwrap();
    reference["cases"]
        .as_array_mut()
        .unwrap()
        .retain(|case| (case["setup"] == true) == setup);
    crate::pg18_oracle::verify(engine, &reference.to_string());
}

fn predecessor(value: &mut Json) -> usize {
    let Json::Object(object) = value else {
        return value
            .as_array_mut()
            .map_or(0, |items| items.iter_mut().map(predecessor).sum());
    };
    let count: usize = object.values_mut().map(predecessor).sum();
    let Some(call) = object.get_mut("Func") else {
        return count;
    };
    let Some(operator) = call["binding"]["dispatch"]["Pattern"]["operator"].as_str() else {
        return count;
    };
    let (mut name, negative) = match operator {
        "Like" => ("like", false),
        "NotLike" => ("like", true),
        "ILike" => ("ilike", false),
        "NotILike" => ("ilike", true),
        "Regex" => ("regex_match_op", false),
        "NotRegex" => ("regex_match_op", true),
        "RegexInsensitive" => ("regex_imatch_op", false),
        "NotRegexInsensitive" => ("regex_imatch_op", true),
        other => panic!("unexpected native pattern {other}"),
    };
    let arguments = call["args"].as_array_mut().unwrap();
    if let Some(similar) =
        arguments[1]["Func"]["binding"]["dispatch"]["PatternEscape"]["similar"].as_bool()
    {
        if similar {
            name = "similar_to";
        }
        let mut helper = arguments.pop().unwrap();
        arguments.append(helper["Func"]["args"].as_array_mut().unwrap());
    }
    call["name"] = name.into();
    call["binding"] = Json::Null;
    if negative {
        *value = json!({"Not": value.take()});
    }
    count + 1
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
    for mut row in catalog.load_foreign_tables().unwrap() {
        row.columns_json = predecessor_json(&row.columns_json, &mut count);
        catalog.save_foreign_table(&row).unwrap();
    }
    assert!(count >= 27, "only {count} native predecessor expressions");
}

fn definitions(catalog: &dyn CatalogFacade) -> Vec<String> {
    let mut definitions = Vec::new();
    for row in catalog.load_tables().unwrap() {
        definitions.push(row.columns_json);
        definitions.push(row.constraints_json);
    }
    for row in catalog.load_views().unwrap() {
        definitions.push(row.definition_json);
    }
    for key in ["sql_functions_json", "sql_rules_json", "sql_triggers_json"] {
        definitions.push(catalog.get_metadata(key).unwrap().unwrap());
    }
    definitions.extend(
        catalog
            .metadata_with_prefix("uqa.sql.domain.v1:")
            .unwrap()
            .into_iter()
            .map(|(_, source)| source),
    );
    for row in catalog.load_catalog_indexes().unwrap() {
        definitions.push(row.columns_json);
        definitions.extend(row.definition_json);
    }
    for row in catalog.load_foreign_tables().unwrap() {
        definitions.push(row.columns_json);
    }
    definitions.sort();
    definitions
}

fn contains_predecessor(value: &Json) -> bool {
    match value {
        Json::Object(fields) => {
            if let Some(call) = fields.get("Func") {
                if call["binding"]["dispatch"].is_null()
                    && matches!(
                        call["name"].as_str(),
                        Some(
                            "like" | "ilike" | "similar_to" | "regex_match_op" | "regex_imatch_op"
                        )
                    )
                {
                    return true;
                }
            }
            fields.values().any(contains_predecessor)
        }
        Json::Array(items) => items.iter().any(contains_predecessor),
        _ => false,
    }
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
fn predecessor_patterns_restore_retained_definitions_once(#[case] storage: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("pattern-predecessor.db");
    let factory = provider(storage, &path);
    let engine = Engine::from_persistent_provider(Arc::clone(&factory)).unwrap();
    verify(&engine, true);
    verify(&engine, false);
    drop(engine);
    {
        let session = factory.open_session().unwrap();
        session.backend.begin_transaction().unwrap();
        downgrade(session.catalog.as_ref());
        session.backend.commit_transaction().unwrap();
    }
    let mut previous = None;
    for _ in 0..2 {
        let engine = Engine::from_persistent_provider(Arc::clone(&factory)).unwrap();
        verify(&engine, false);
        let peer = engine.new_session().unwrap();
        verify(&peer, false);
        drop(peer);
        drop(engine);
        let session = factory.open_session().unwrap();
        let current = definitions(session.catalog.as_ref());
        for source in &current {
            assert!(
                !contains_predecessor(&serde_json::from_str(source).unwrap()),
                "predecessor marker survived restoration"
            );
        }
        if let Some(previous) = &previous {
            assert_eq!(
                previous, &current,
                "second open rewrote retained definitions"
            );
        }
        previous = Some(current);
    }
}

#[rstest::rstest]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn secondary_sessions_cannot_publish_pattern_migrations(#[case] storage: usize) {
    let directory = tempfile::tempdir().unwrap();
    let factory = provider(storage, &directory.path().join("pattern-load-only.db"));
    let engine = Engine::from_persistent_provider(Arc::clone(&factory)).unwrap();
    verify(&engine, true);
    let before = {
        let session = factory.open_session().unwrap();
        session.backend.begin_transaction().unwrap();
        downgrade(session.catalog.as_ref());
        session.backend.commit_transaction().unwrap();
        definitions(session.catalog.as_ref())
    };
    let error = engine
        .new_session()
        .err()
        .expect("peer must not migrate the durable catalog");
    assert!(error.to_string().contains("initial-open"), "{error}");
    let session = factory.open_session().unwrap();
    assert_eq!(before, definitions(session.catalog.as_ref()));
    drop(session);
    drop(engine);
    let engine = Engine::from_persistent_provider(factory).unwrap();
    verify(&engine, false);
}
