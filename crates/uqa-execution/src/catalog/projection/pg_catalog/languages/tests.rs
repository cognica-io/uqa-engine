//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::{
    projection::pg_proc::build_pg_proc, test_support::empty_catalog, CatalogReadView,
    RelationLookupMode, RelationNameResolution,
};
use std::sync::Arc;
use uqa_sql::{
    ast::Statement,
    routines::{RoutineBody, SQLUserFunction},
};

fn reference(id: &str) -> serde_json::Value {
    let fixture: serde_json::Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/parity/pg18/finite_extraction_oracle.expected.json"
    )))
    .unwrap();
    fixture["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["id"] == id)
        .unwrap()["results"]
        .as_array()
        .unwrap()
        .last()
        .unwrap()
        .clone()
}

fn wire_value(value: &Value) -> serde_json::Value {
    match value {
        Value::Null => serde_json::Value::Null,
        Value::Int(value) => value.to_string().into(),
        Value::Str(value) => value.clone().into(),
        Value::Bool(value) => if *value { "t" } else { "f" }.into(),
        value => panic!("unexpected language catalog value: {value:?}"),
    }
}

fn assert_rows(id: &str, rows: &[ResultRow]) {
    let expected = reference(id);
    let actual = rows
        .iter()
        .map(|row| {
            expected["columns"]
                .as_array()
                .unwrap()
                .iter()
                .map(|column| wire_value(&row[column.as_str().unwrap()]))
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    assert_eq!(serde_json::json!(actual), expected["rows"], "{id}");
}

#[test]
fn language_rows_and_attribute_descriptors_match_postgresql() {
    assert_rows("language_rows", &build_pg_language());
    assert_rows("language_columns", &language_attribute_rows().unwrap());
}

#[test]
fn language_row_and_array_types_share_the_catalog_relation_identity() {
    let catalog = empty_catalog();
    let relation = language_class_row(&catalog);
    let types = language_type_rows(&catalog);
    let [composite, array] = types.as_slice() else {
        panic!("row type and companion array");
    };
    let expected = reference("language_relation");
    let actual = [
        &relation["oid"],
        &relation["relname"],
        &relation["reltype"],
        &relation["relnatts"],
        &composite["oid"],
        &composite["typname"],
        &composite["typrelid"],
        &composite["typarray"],
        &composite["typtype"],
    ]
    .map(wire_value);
    assert_eq!(serde_json::json!(actual), expected["rows"][0]);
    assert_eq!(array["oid"], composite["typarray"]);
    assert_eq!(array["typelem"], composite["oid"]);
    assert_eq!(array["typrelid"], Value::Int(0));
    assert_eq!(array["typname"], Value::Str("_pg_language".into()));
    assert_eq!(array["typowner"], Value::Int(RoleIdentity::BOOTSTRAP.oid));
}

#[test]
fn user_sql_and_procedural_routines_join_the_same_language_identities() {
    let mut snapshot = empty_catalog().snapshot().clone();
    for (sql, oid) in [
        ("CREATE FUNCTION public.extraction_sql_identity() RETURNS integer LANGUAGE SQL AS 'SELECT 1'", 20_001),
        ("CREATE FUNCTION public.extraction_plpgsql_identity() RETURNS integer LANGUAGE plpgsql AS 'BEGIN RETURN 2; END'", 20_002),
    ] {
        let Statement::CreateFunction(mut definition) = uqa_sql::compile(sql).unwrap().remove(0) else {
            panic!("routine definition");
        };
        definition.catalog_oid = Some(oid);
        definition.owner = Some(RoleIdentity::BOOTSTRAP);
        Arc::make_mut(&mut snapshot.definitions.sql_user_functions).insert(
            definition.name.clone(),
            vec![Arc::new(SQLUserFunction::new(*definition, RoutineBody::Source))],
        );
    }
    let catalog = CatalogReadView::new(snapshot);
    let resolution = RelationNameResolution {
        search_path: vec!["public".into()],
        current_user: "uqa".into(),
        temporary_schema: "pg_temp_1".into(),
        temporary_namespace_allocated: false,
        lookup_mode: RelationLookupMode::Bound,
    };
    let languages = build_pg_language();
    let mut rows = build_pg_proc(None, &catalog, &resolution)
        .unwrap()
        .into_iter()
        .filter_map(|mut routine| {
            if !matches!(routine["oid"], Value::Int(20_001 | 20_002)) {
                return None;
            }
            let language = languages
                .iter()
                .find(|language| language["oid"] == routine["prolang"])
                .expect("routine language exists");
            routine.insert("lanname".into(), language["lanname"].clone());
            Some(routine)
        })
        .collect::<Vec<_>>();
    rows.sort_by(|left, right| left["proname"].cmp(&right["proname"]));
    assert_rows("routine_language_join", &rows);
}
