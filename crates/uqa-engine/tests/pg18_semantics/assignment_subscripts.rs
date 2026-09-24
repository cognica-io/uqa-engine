//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Provider, preparation and atomicity acceptance against independently captured PostgreSQL rows and diagnostics.

use std::{path::Path, sync::Arc};
use uqa_core::Value;
use uqa_engine::{Engine, SQLResult};

fn open(provider: usize, path: &Path) -> Engine {
    match provider {
        0 => Engine::open(path).unwrap(),
        1 => Engine::from_persistent_provider(Arc::new(
            uqa_storage_sqlite::SQLiteKeyValueStorage::open(path).unwrap(),
        ))
        .unwrap(),
        2 => Engine::from_persistent_provider(Arc::new(
            uqa_storage_redb::RedbStorage::open(path).unwrap(),
        ))
        .unwrap(),
        _ => unreachable!(),
    }
}

fn rows(result: SQLResult) -> serde_json::Value {
    serde_json::Value::Array(
        result
            .rows
            .into_iter()
            .map(|row| {
                serde_json::Value::Object(
                    row.into_iter()
                        .map(|(key, value)| {
                            let value = match value {
                                Value::Null => serde_json::Value::Null,
                                Value::Int(value) => value.into(),
                                Value::Str(value) => value.into(),
                                other => panic!("unexpected oracle projection {other:?}"),
                            };
                            (key, value)
                        })
                        .collect(),
                )
            })
            .collect(),
    )
}

const STORED: &str = "SELECT id, value::text AS value, array_dims(value) AS dimensions FROM assignment_target ORDER BY id";

#[rstest::rstest]
#[case::element("element")]
#[case::element_null("element_null")]
#[case::extend_before("extend_before")]
#[case::extend_after("extend_after")]
#[case::slice("slice")]
#[case::slice_lower_omitted("slice_lower_omitted")]
#[case::slice_upper_omitted("slice_upper_omitted")]
#[case::slice_null("slice_null")]
#[case::element_array_rhs("element_array_rhs")]
#[case::slice_scalar_rhs("slice_scalar_rhs")]
#[case::null_index("null_index")]
#[case::text_index("text_index")]
#[case::reversed_slice("reversed_slice")]
#[case::null_array("null_array")]
#[case::empty_array("empty_array")]
#[case::unvisited_null_index("unvisited_null_index")]
#[case::repeated_element_targets("repeated_element_targets")]
#[case::whole_and_element_targets("whole_and_element_targets")]
#[case::multidimensional_element("multidimensional_element")]
#[case::multidimensional_slice("multidimensional_slice")]
#[case::multidimensional_flat_slice_source("multidimensional_flat_slice_source")]
#[case::multidimensional_omitted_dimension("multidimensional_omitted_dimension")]
#[case::multidimensional_extension("multidimensional_extension")]
#[case::wrong_subscript_count("wrong_subscript_count")]
#[case::shifted_multidimensional_element("shifted_multidimensional_element")]
#[case::slice_extends_with_gap("slice_extends_with_gap")]
#[case::empty_slice_missing_bound("empty_slice_missing_bound")]
#[case::empty_zero_width_slice("empty_zero_width_slice")]
#[case::empty_negative_width_slice("empty_negative_width_slice")]
#[case::empty_lower_bound_limit("empty_lower_bound_limit")]
#[case::empty_source_error_precedence("empty_source_error_precedence")]
#[case::nonempty_bounds_error_precedence("nonempty_bounds_error_precedence")]
#[case::insert_element("insert_element")]
#[case::insert_repeated_elements("insert_repeated_elements")]
#[case::insert_whole_and_element("insert_whole_and_element")]
#[case::insert_select("insert_select")]
#[case::conflict_element("conflict_element")]
#[case::merge_update("merge_update")]
#[case::merge_insert("merge_insert")]
#[case::int2vector_element("int2vector_element")]
#[case::oidvector_element("oidvector_element")]
#[case::domain_accepts("domain_accepts")]
#[case::domain_rejects("domain_rejects")]
#[case::bound_reads_original_row("bound_reads_original_row")]
#[case::repeated_same_element("repeated_same_element")]
#[case::decimal_index_rounding("decimal_index_rounding")]
#[case::bound_scalar_subquery("bound_scalar_subquery")]
#[case::update_from_bounds("update_from_bounds")]
#[case::mixed_slice_and_index("mixed_slice_and_index")]
#[case::insert_ignores_whole_column_default("insert_ignores_whole_column_default")]
#[case::partial_default_rejected("partial_default_rejected")]
#[case::null_slice_null_bound("null_slice_null_bound")]
#[case::domain_repeated_elements("domain_repeated_elements")]
#[case::domain_null_slice_insert("domain_null_slice_insert")]
#[case::automatic_view_repeated_elements("automatic_view_repeated_elements")]
#[case::automatic_view_insert("automatic_view_insert")]
fn assignment_subscripts_match_pg18(
    #[case] name: &str,
    #[values(0, 1, 2)] provider: usize,
    #[values(false, true)] prepared: bool,
) {
    let oracle: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../tests/parity/pg18/assignment_subscripts.expected.json"
    ))
    .unwrap();
    let case = oracle["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["name"] == name)
        .unwrap();
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("assignments.db");
    let engine = open(provider, &path);
    for sql in case["setup"].as_array().unwrap() {
        let sql = sql.as_str().unwrap();
        engine
            .sql(sql, &[])
            .unwrap_or_else(|error| panic!("{name}: {sql}: {error}"));
    }
    let before = rows(engine.sql(STORED, &[]).unwrap());
    let sql = case["sql"].as_str().unwrap();
    let result = if prepared {
        engine
            .sql(&format!("PREPARE assignment_probe AS {sql}"), &[])
            .and_then(|_| engine.sql("EXECUTE assignment_probe", &[]))
    } else {
        engine.sql(sql, &[])
    };
    if let Some(expected) = case.get("error") {
        let error = result.unwrap_err();
        assert_eq!(
            error.sqlstate(),
            expected["sqlstate"].as_str(),
            "{name}: {error}"
        );
        assert_eq!(
            error.to_string(),
            expected["message"].as_str().unwrap(),
            "{name}"
        );
        let (detail, hint) = match &error {
            uqa_sql::SQLError::Diagnostic { detail, hint, .. } => {
                (detail.as_deref(), hint.as_deref())
            }
            _ => (None, None),
        };
        assert_eq!(
            detail,
            expected.get("detail").and_then(serde_json::Value::as_str),
            "{name}"
        );
        assert_eq!(
            hint,
            expected.get("hint").and_then(serde_json::Value::as_str),
            "{name}"
        );
        assert_eq!(
            rows(engine.sql(STORED, &[]).unwrap()),
            before,
            "failed {name} changed the row"
        );
    } else {
        assert_eq!(
            rows(result.unwrap_or_else(|error| panic!("{name}: {error}"))),
            case["rows"],
            "{name}"
        );
    }
    let stored = rows(engine.sql(STORED, &[]).unwrap());
    drop(engine);
    let reopened = open(provider, &path);
    assert_eq!(
        rows(reopened.sql(STORED, &[]).unwrap()),
        stored,
        "{name} lost its dimensions on reopen"
    );
}
