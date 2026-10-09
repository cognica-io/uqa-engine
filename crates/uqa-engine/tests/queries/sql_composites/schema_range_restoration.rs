//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Durable syntax consumers retain independently evaluated range comparisons.

use serde_json::{json, Value as Json};
use uqa_engine::Engine;
use uqa_storage::CatalogFacade;

fn reference() -> Json {
    serde_json::from_str(include_str!(
        "../../../../../tests/parity/pg18/schema_range_restoration_oracle.expected.json"
    ))
    .unwrap()
}

fn setup(engine: &Engine) {
    let reference = reference();
    let setup = reference["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["id"] == "setup")
        .unwrap();
    engine.sql(setup["sql"].as_str().unwrap(), &[]).unwrap();
}

fn verify(engine: &Engine) {
    let mut reference = reference();
    reference["cases"]
        .as_array_mut()
        .unwrap()
        .retain(|case| !matches!(case["id"].as_str(), Some("version" | "setup" | "cleanup")));
    crate::pg18_oracle::verify(engine, &reference.to_string());
}

fn predecessor(value: &mut Json) -> usize {
    let Json::Object(object) = value else {
        return value
            .as_array_mut()
            .map_or(0, |items| items.iter_mut().map(predecessor).sum());
    };
    let mut count = object.values_mut().map(predecessor).sum();
    if let Some(items) = object.get("And").and_then(Json::as_array) {
        if items.len() == 2
            && items[0]["Binary"]["op"] == "GreaterEqual"
            && items[1]["Binary"]["op"] == "LessEqual"
            && items[0]["Binary"]["lhs"] == items[1]["Binary"]["lhs"]
        {
            *value = json!({"Between": {
                "expr": items[0]["Binary"]["lhs"],
                "low": items[0]["Binary"]["rhs"],
                "high": items[1]["Binary"]["rhs"]
            }});
            return count + 1;
        }
    }
    if let Some(items) = object.get("Or").and_then(Json::as_array) {
        if items.len() == 2
            && items[0].get("Between").is_some()
            && items[1].get("Between").is_some()
        {
            let operands = &items[0]["Between"];
            let binding = uqa_sql::ast::FunctionBinding::dispatched(
                uqa_sql::ast::FunctionDispatch::BetweenSymmetric,
            );
            *value = json!({"Func": {
                "name": binding.name,
                "binding": binding,
                "args": [operands["expr"], operands["low"], operands["high"]],
                "distinct": false,
                "order_by": [],
                "filter": null,
                "order_syntax": "Ordinary"
            }});
            count += 1;
        }
    }
    count
}

fn predecessor_json(source: &str, count: &mut usize) -> String {
    let mut value: Json = serde_json::from_str(source).unwrap();
    *count += predecessor(&mut value);
    value.to_string()
}

fn predecessor_catalog(catalog: &dyn CatalogFacade) {
    let mut count = 0;
    for mut row in catalog.load_tables().unwrap() {
        row.columns_json = predecessor_json(&row.columns_json, &mut count);
        row.constraints_json = predecessor_json(&row.constraints_json, &mut count);
        catalog.save_table(&row).unwrap();
    }
    for key in ["sql_functions_json", "sql_rules_json", "sql_triggers_json"] {
        if let Some(source) = catalog.get_metadata(key).unwrap() {
            catalog
                .set_metadata(key, &predecessor_json(&source, &mut count))
                .unwrap();
        }
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
    assert!(count >= 24, "only {count} predecessor nodes");
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn fresh_schema_ranges_match_postgresql(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let engine = super::addition::open(provider, &directory.path().join("schema-ranges.db"));
    setup(&engine);
    verify(&engine);
}

#[rstest::rstest]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn predecessor_schema_ranges_match_postgresql_after_reopening(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("schema-ranges.db");
    let engine = super::addition::open(provider, &path);
    setup(&engine);
    drop(engine);
    super::addition::restoration::catalog(provider, &path, predecessor_catalog);
    let mut durable = None;
    for _ in 0..2 {
        let engine = super::addition::open(provider, &path);
        verify(&engine);
        drop(engine);
        super::addition::restoration::catalog(provider, &path, |catalog| {
            let current = catalog_snapshot(catalog);
            for row in &current {
                assert!(
                    !row.contains("BetweenSymmetric") && !row.contains("\"Between\""),
                    "predecessor syntax: {row}"
                );
            }
            if let Some(before) = &durable {
                assert_eq!(before, &current);
            }
            durable = Some(current);
        });
    }
}

fn catalog_snapshot(catalog: &dyn CatalogFacade) -> Vec<String> {
    let mut records = Vec::new();
    for row in catalog.load_foreign_tables().unwrap() {
        records.push(row.columns_json);
    }
    for row in catalog.load_tables().unwrap() {
        records.push(format!("{} {}", row.columns_json, row.constraints_json));
    }
    for row in catalog.load_catalog_indexes().unwrap() {
        records.push(format!(
            "{} {}",
            row.columns_json,
            row.definition_json.unwrap_or_default()
        ));
    }
    for key in ["sql_functions_json", "sql_rules_json", "sql_triggers_json"] {
        if let Some(value) = catalog.get_metadata(key).unwrap() {
            records.push(value);
        }
    }
    records.extend(
        catalog
            .metadata_with_prefix("uqa.sql.domain.v1:")
            .unwrap()
            .into_iter()
            .map(|(_, value)| value),
    );
    records.sort();
    records
}

#[rstest::rstest]
#[case::tables("tables")]
#[case::routines("sql_functions_json")]
#[case::rules("sql_rules_json")]
#[case::rule_plans("rule_plans")]
#[case::triggers("sql_triggers_json")]
#[case::domains("domains")]
#[case::indexes("indexes")]
#[case::partitions("partitions")]
#[case::foreign_tables("foreign")]
fn secondary_sessions_cannot_publish_expression_migrations(#[case] kind: &str) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("schema-range-load-only.db");
    let engine = super::addition::open(1, &path);
    setup(&engine);
    let before = super::addition::restoration::catalog(1, &path, |catalog| {
        let mut count = 0;
        match kind {
            "rule_plans" => {
                let source = catalog.get_metadata("sql_rules_json").unwrap().unwrap();
                let mut value: Json = serde_json::from_str(&source).unwrap();
                for rule in value["rules"].as_array_mut().unwrap() {
                    if !rule["definition"]["condition"].is_null() {
                        rule["condition_plan"] = Json::Null;
                        rule["condition_binding"] = Json::Null;
                        count += 1;
                    }
                }
                catalog
                    .set_metadata("sql_rules_json", &value.to_string())
                    .unwrap();
            }
            "tables" => {
                for mut row in catalog.load_tables().unwrap() {
                    row.columns_json = predecessor_json(&row.columns_json, &mut count);
                    catalog.save_table(&row).unwrap();
                }
            }
            "partitions" => {
                for mut row in catalog.load_tables().unwrap() {
                    let mut constraints: Json =
                        serde_json::from_str(&row.constraints_json).unwrap();
                    count += predecessor(&mut constraints["hierarchy"]);
                    row.constraints_json = constraints.to_string();
                    catalog.save_table(&row).unwrap();
                }
            }
            "indexes" => {
                for mut row in catalog.load_catalog_indexes().unwrap() {
                    row.columns_json = predecessor_json(&row.columns_json, &mut count);
                    row.definition_json = row
                        .definition_json
                        .as_deref()
                        .map(|source| predecessor_json(source, &mut count));
                    catalog.save_catalog_index_row(&row).unwrap();
                }
            }
            "domains" => {
                for (key, source) in catalog.metadata_with_prefix("uqa.sql.domain.v1:").unwrap() {
                    catalog
                        .set_metadata(&key, &predecessor_json(&source, &mut count))
                        .unwrap();
                }
            }
            "foreign" => {
                for mut row in catalog.load_foreign_tables().unwrap() {
                    row.columns_json = predecessor_json(&row.columns_json, &mut count);
                    catalog.save_foreign_table(&row).unwrap();
                }
            }
            key => {
                let source = catalog.get_metadata(key).unwrap().unwrap();
                let source = predecessor_json(&source, &mut count);
                catalog.set_metadata(key, &source).unwrap();
            }
        }
        assert!(count > 0, "{kind} must contain predecessor syntax");
        catalog_snapshot(catalog)
    });
    let Err(error) = engine.new_session() else {
        panic!("secondary session migrated {kind}")
    };
    assert!(error.to_string().contains("migration"), "{kind}: {error}");
    super::addition::restoration::catalog(1, &path, |catalog| {
        assert_eq!(catalog_snapshot(catalog), before);
    });
    drop(engine);
    verify(&super::addition::open(1, &path));
}

#[rstest::rstest]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn failed_schema_migration_retains_all_predecessor_records(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("schema-range-rollback.db");
    let engine = super::addition::open(provider, &path);
    setup(&engine);
    drop(engine);
    let (rules, before) = super::addition::restoration::catalog(provider, &path, |catalog| {
        predecessor_catalog(catalog);
        let rules = catalog.get_metadata("sql_rules_json").unwrap().unwrap();
        let mut future: Json = serde_json::from_str(&rules).unwrap();
        future["format_version"] = json!(u32::MAX);
        catalog
            .set_metadata("sql_rules_json", &future.to_string())
            .unwrap();
        (rules, catalog_snapshot(catalog))
    });
    let result = match provider {
        1 => Engine::open(&path).map_err(|error| error.to_string()),
        2 => Engine::from_persistent_provider(std::sync::Arc::new(
            uqa_storage_sqlite::SQLiteKeyValueStorage::open(&path).unwrap(),
        ))
        .map_err(|error| error.to_string()),
        3 => Engine::from_persistent_provider(std::sync::Arc::new(
            uqa_storage_redb::RedbStorage::open(&path).unwrap(),
        ))
        .map_err(|error| error.to_string()),
        _ => unreachable!(),
    };
    let Err(error) = result else {
        panic!("future rule catalog must fail restoration")
    };
    assert!(error.contains("newer than supported"), "{error}");
    super::addition::restoration::catalog(provider, &path, |catalog| {
        assert_eq!(catalog_snapshot(catalog), before);
        catalog.set_metadata("sql_rules_json", &rules).unwrap();
    });
    verify(&super::addition::open(provider, &path));
}
