//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Provider, preparation and atomicity acceptance against independently captured `PostgreSQL` rows and diagnostics.

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

#[test]
fn unknown_literal_view_columns_reject_subscripts_before_rewrite() {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE VIEW assignment_unknown AS SELECT NULL AS value",
            &[],
        )
        .unwrap();
    engine.sql("CREATE RULE assignment_unknown_update AS ON UPDATE TO assignment_unknown DO INSTEAD NOTHING", &[]).unwrap();
    let error = engine
        .sql("UPDATE assignment_unknown SET value[1] = 9", &[])
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("42804"));
    assert_eq!(
        error.to_string(),
        "cannot subscript type text because it does not support subscripting"
    );
}

#[test]
fn untyped_callback_view_rejects_partial_assignments_before_effects() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let engine = Engine::new();
    let effects = Arc::new(AtomicUsize::new(0));
    let captured = Arc::clone(&effects);
    engine
        .register_scalar_function("assignment_effect", move |_: &[Value]| {
            captured.fetch_add(1, Ordering::SeqCst);
            Ok(Value::Int(9))
        })
        .unwrap();
    engine
        .register_scalar_function("assignment_untyped_array", |_: &[Value]| {
            Ok(Value::Array(
                uqa_core::ArrayValue::try_new(vec![Value::Int(1), Value::Int(2)]).unwrap(),
            ))
        })
        .unwrap();
    for sql in [
        "CREATE VIEW assignment_untyped AS SELECT assignment_untyped_array() AS value",
        "CREATE FUNCTION assignment_receive() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN PERFORM assignment_effect(); RETURN NEW; END $$",
        "CREATE TRIGGER assignment_receive INSTEAD OF INSERT OR UPDATE ON assignment_untyped FOR EACH ROW EXECUTE FUNCTION assignment_receive()",
    ] {
        engine.sql(sql, &[]).unwrap();
    }
    for sql in [
        "UPDATE assignment_untyped SET value[1] = assignment_effect()",
        "UPDATE assignment_untyped SET value[1:2] = ARRAY[assignment_effect()] WHERE false",
        "INSERT INTO assignment_untyped (value[1]) VALUES (assignment_effect())",
        "INSERT INTO assignment_untyped (value[1]) SELECT assignment_effect()",
        "MERGE INTO assignment_untyped USING (VALUES (1)) AS s(id) ON true WHEN MATCHED THEN UPDATE SET value[1] = assignment_effect()",
        "MERGE INTO assignment_untyped USING (VALUES (1)) AS s(id) ON false WHEN NOT MATCHED THEN INSERT (value[1]) VALUES (assignment_effect())",
    ] {
        for prepared in [false, true] {
            let sql = if prepared {
                format!("PREPARE assignment_untyped_probe AS {sql}")
            } else {
                sql.to_owned()
            };
            let error = engine.sql(&sql, &[]).unwrap_err();
            assert_eq!(error.sqlstate(), Some("42804"), "{sql}: {error}");
            assert_eq!(error.to_string(), "cannot subscript type unknown because it does not support subscripting", "{sql}");
            assert_eq!(effects.load(Ordering::SeqCst), 0, "{sql}");
        }
    }
    engine
        .sql("UPDATE assignment_untyped SET value = ARRAY[9]", &[])
        .unwrap();
    assert_eq!(effects.load(Ordering::SeqCst), 1);
}

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
#[case::stored_function_element("stored_function_element")]
#[case::stored_function_target_rename("stored_function_target_rename")]
#[case::stored_function_bound_rename("stored_function_bound_rename")]
#[case::stored_function_bound_routine_rename("stored_function_bound_routine_rename")]
#[case::stored_rule_element("stored_rule_element")]
#[case::stored_rule_target_rename("stored_rule_target_rename")]
#[case::generated_value_after_partial_assignment("generated_value_after_partial_assignment")]
#[case::domain_multiple_rows_atomic("domain_multiple_rows_atomic")]
#[case::unique_multiple_rows_atomic("unique_multiple_rows_atomic")]
#[case::trigger_assignment_failure("trigger_assignment_failure")]
#[case::unique_expression_column_privileges("unique_expression_column_privileges")]
#[case::unique_expression_table_privilege("unique_expression_table_privilege")]
#[case::unique_column_privileges("unique_column_privileges")]
#[case::unique_real_output("unique_real_output")]
#[case::unique_fixed_char_output("unique_fixed_char_output")]
#[case::unique_regclass_output("unique_regclass_output")]
#[case::unique_int2vector_output("unique_int2vector_output")]
#[case::unique_oidvector_output("unique_oidvector_output")]
#[case::unique_boolean_output("unique_boolean_output")]
#[case::unique_null_output("unique_null_output")]
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
    let engine = if case["reopen_before"] == true {
        drop(engine);
        open(provider, &path)
    } else {
        engine
    };
    let stored_sql = case["stored_sql"].as_str().unwrap_or(STORED);
    let before = rows(engine.sql(stored_sql, &[]).unwrap());
    if let Some(role) = case["role"].as_str() {
        engine.sql(&format!("SET ROLE {role}"), &[]).unwrap();
    }
    let savepoint = case["savepoint"].as_bool().unwrap_or(false);
    if savepoint {
        engine.sql("BEGIN; SAVEPOINT assignment_undo", &[]).unwrap();
    }
    let sql = case["sql"].as_str().unwrap();
    let result = if prepared {
        engine
            .sql(&format!("PREPARE assignment_probe AS {sql}"), &[])
            .and_then(|_| engine.sql("EXECUTE assignment_probe", &[]))
    } else {
        engine.sql(sql, &[])
    };
    if savepoint {
        engine
            .sql(
                if result.is_err() {
                    "ROLLBACK TO assignment_undo; COMMIT"
                } else {
                    "COMMIT"
                },
                &[],
            )
            .unwrap();
    }
    if case["role"].is_string() {
        engine.sql("RESET ROLE", &[]).unwrap();
    }
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
            rows(engine.sql(stored_sql, &[]).unwrap()),
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
    let stored = rows(engine.sql(stored_sql, &[]).unwrap());
    if let Some(expected) = case.get("stored") {
        assert_eq!(
            &stored, expected,
            "{name} stored a different row than PostgreSQL"
        );
    }
    drop(engine);
    let reopened = open(provider, &path);
    assert_eq!(
        rows(reopened.sql(stored_sql, &[]).unwrap()),
        stored,
        "{name} lost its dimensions on reopen"
    );
}
