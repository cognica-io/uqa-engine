//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::test_support::{empty_catalog, CatalogServices};
use std::collections::BTreeSet;
use std::sync::Arc;
use uqa_sql::catalog::{roles::RoleDefinition, security::builtin_routines::BuiltinRoutineSecurity};

/// The bootstrap catalog with an ordinary `reader` role, where the bootstrap owner revoked `EXECUTE` on `revoked` from PUBLIC.
fn catalog(revoked: &[u32]) -> CatalogReadView {
    let mut snapshot = empty_catalog().snapshot().clone();
    let mut reader = RoleDefinition::bootstrap();
    reader.name = "reader".into();
    reader.oid = 20_000;
    reader.object_id = [1; 16];
    reader.attributes.clear();
    snapshot.definitions.roles = Arc::new(
        [
            ("uqa".into(), RoleDefinition::bootstrap()),
            (reader.name.clone(), reader),
        ]
        .into(),
    );
    snapshot.definitions.builtin_routine_security = Arc::new(
        revoked
            .iter()
            .map(|oid| {
                (
                    *oid,
                    BuiltinRoutineSecurity {
                        revision: [1; 16],
                        execute_acl: vec![RoutineAclEntry {
                            role: Some(RoleIdentity::BOOTSTRAP),
                            grantor: RoleIdentity::BOOTSTRAP,
                            grant_option: false,
                        }],
                    },
                )
            })
            .collect(),
    );
    CatalogReadView::new(snapshot)
}

fn routines(catalog: &CatalogReadView, user: &str) -> Vec<ResultRow> {
    let mut services = CatalogServices::default();
    services.resolution.current_user = user.into();
    build_info_routines(catalog, &services.resolution).unwrap()
}

fn text(row: &ResultRow, column: &str) -> Option<String> {
    match row.get(column) {
        None | Some(Value::Null) => None,
        Some(Value::Str(value)) => Some(value.clone()),
        Some(other) => panic!("{column} is not text: {other:?}"),
    }
}

#[test]
fn builtin_result_type_columns_follow_the_postgresql_view_definition() {
    let reference: serde_json::Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/parity/pg18/regular_expression_identities_oracle.expected.json"
    )))
    .unwrap();
    let projected = routines(&catalog(&[]), "uqa");
    assert_eq!(
        assert_transcript_rows(&reference, "routines_return_types", &projected),
        48
    );
    assert_eq!(
        assert_transcript_rows(&reference, "routines_types", &projected),
        29
    );
}

/// Compare a transcript case's rows with the projected routines its first column names, and count them.
fn assert_transcript_rows(
    reference: &serde_json::Value,
    id: &str,
    projected: &[ResultRow],
) -> usize {
    let result = &reference["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["id"] == id)
        .unwrap()["results"][0];
    let columns = result["columns"].as_array().unwrap();
    let rows = result["rows"].as_array().unwrap();
    for expected in rows {
        let name = expected[0].as_str().unwrap();
        let row = projected
            .iter()
            .find(|row| text(row, "specific_name").as_deref() == Some(name))
            .unwrap_or_else(|| panic!("missing routine {name}"));
        for (column, expected) in columns.iter().zip(expected.as_array().unwrap()) {
            let column = column.as_str().unwrap();
            let actual = if column == "current_catalog" {
                Some(
                    if row.get("type_udt_catalog") == Some(&catalog_name()) {
                        "t"
                    } else {
                        "f"
                    }
                    .to_owned(),
                )
            } else {
                text(row, column)
            };
            assert_eq!(actual.as_deref(), expected.as_str(), "{name} {column}");
        }
    }
    rows.len()
}

#[test]
fn routine_rows_and_definitions_require_owner_usage_or_execute() {
    let catalog = catalog(&[2763, 2766]);
    let names = |rows: &[ResultRow]| {
        rows.iter()
            .filter_map(|row| text(row, "specific_name"))
            .collect::<BTreeSet<_>>()
    };
    let owner = routines(&catalog, "uqa");
    let reader = routines(&catalog, "reader");
    assert_eq!(
        names(&owner)
            .difference(&names(&reader))
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["regexp_matches_2763", "regexp_split_to_table_2766"]
    );
    let definition = |rows: &[ResultRow], name: &str| {
        rows.iter()
            .find(|row| text(row, "specific_name").as_deref() == Some(name))
            .and_then(|row| text(row, "routine_definition"))
    };
    assert_eq!(
        definition(&owner, "regexp_matches_2764").as_deref(),
        Some("regexp_matches")
    );
    assert_eq!(definition(&reader, "regexp_matches_2764"), None);
    assert!(names(&reader).contains("regexp_matches_2764"));
}
