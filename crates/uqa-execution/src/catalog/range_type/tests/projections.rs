//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::{cache::RegtypeOutputCache, projection, test_support::CatalogServices};
use uqa_core::Value;

#[test]
fn declared_range_catalog_rows_match_postgresql_type_range_and_dependency_rows() {
    let fixture: serde_json::Value = serde_json::from_str(include_str!("postgresql.json")).unwrap();
    let catalog = catalog(definition());
    let services = CatalogServices::default();
    let cache = RegtypeOutputCache::default();
    let context = services.context(&catalog, &cache);
    for case in fixture["cases"].as_array().unwrap() {
        let id = case["id"].as_str().unwrap();
        let table = match id {
            "types" => "pg_type",
            "range" => "pg_range",
            "dependencies" => "pg_depend",
            _ => unreachable!(),
        };
        let key = match id {
            "types" => "oid",
            "range" => "rngtypid",
            "dependencies" => "objid",
            _ => unreachable!(),
        };
        let rows = projection::build_info_schema_rows(
            &context,
            &catalog,
            &services.resolution,
            &services,
            &format!("pg_catalog.{table}"),
        )
        .unwrap()
        .unwrap()
        .into_iter()
        .filter(|row| matches!(row.get(key), Some(Value::Int(oid)) if definition().type_oids().map(i64::from).contains(oid)))
        .collect::<Vec<_>>();
        let result = &case["results"][0];
        let mut actual = rows
            .iter()
            .map(|row| {
                result["columns"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|name| {
                        let name = name.as_str().unwrap();
                        match &row[name] {
                            Value::Str(value) => Some(value.clone()),
                            Value::Int(value)
                                if id == "dependencies"
                                    && matches!(name, "classid" | "refclassid") =>
                            {
                                Some(
                                    match value {
                                        1247 => "pg_type",
                                        2615 => "pg_namespace",
                                        _ => panic!("class {value}"),
                                    }
                                    .into(),
                                )
                            }
                            Value::Int(value) => Some(value.to_string()),
                            Value::Bool(value) => Some(if *value { "t" } else { "f" }.into()),
                            Value::Null => None,
                            value => panic!("{name}: {value:?}"),
                        }
                    })
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        let mut expected: Vec<Vec<Option<String>>> =
            serde_json::from_value(result["rows"].clone()).unwrap();
        actual.sort();
        expected.sort();
        assert_eq!(actual, expected, "{id}");
    }
}

#[test]
fn independent_range_names_and_arrays_resolve_by_current_identity_and_parent_privilege() {
    let mut definition = definition();
    let original = definition.multirange_type();
    definition.multirange.identity = RelationIdentity::new("public", "renamed");
    definition.multirange.array_name = "__renamed".into();
    let catalog = catalog(definition.clone());
    let services = CatalogServices::default();
    let cache = RegtypeOutputCache::default();
    let context = services.context(&catalog, &cache);
    assert_eq!(context.resolve_user_type("renamed"), Some(original));
    assert_eq!(
        context.resolve_user_type("__renamed"),
        Some(ColumnType::Array(Box::new(definition.multirange_type())))
    );
    assert!(context.resolve_user_type("r_multirange").is_none());
    for (name, oid) in [
        ("r", definition.oid),
        ("_r", definition.array_oid),
        ("renamed", definition.multirange.oid),
        ("__renamed", definition.multirange.array_oid),
    ] {
        let ty = context.resolve_user_type(name).unwrap();
        assert_eq!(
            uqa_sql::catalog::type_metadata::pg_type_oid(&ty),
            i64::from(oid)
        );
        assert_eq!(
            projection::type_privilege_oid(&context, i64::from(oid)).unwrap(),
            Some(i64::from(definition.oid))
        );
        assert_eq!(
            projection::resolve_catalog_user_type_by_oid(&context, oid),
            Some(ty)
        );
    }
}
