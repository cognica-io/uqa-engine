use super::lowering_expression::strip_assignment_target;
use super::lowering_statement::{lower_stmt, lower_stmt_list};
use super::parsing::{lower_datum, parse_plpgsql_text, validate_datums};
use super::*;

fn scalar_datum(name: &str) -> PLpgSQLDatum {
    PLpgSQLDatum::Var(PLpgSQLVar {
        name: name.into(),
        type_name: "integer".into(),
        default: None,
        constant: false,
        not_null: false,
        lineno: None,
    })
}

fn json_expr(query: &str, mode: i64) -> JSONValue {
    serde_json::json!({
        "PLpgSQL_expr": {
            "query": query,
            "parseMode": mode,
        }
    })
}

#[test]
fn representative_parser_output_lowers_without_silent_defaults() {
    let parsed = parse_plpgsql_text(
        "CREATE FUNCTION audit_shape(x int) RETURNS int AS $$\n\
         DECLARE r record; y int := 1; z int;\n\
         BEGIN\n\
           IF x > 0 THEN y := x; ELSIF x = 0 THEN y := 2; END IF;\n\
           CASE x WHEN 1 THEN y := 3; ELSE y := 4; END CASE;\n\
           FOR y IN 0..2 LOOP CONTINUE WHEN y = 1; END LOOP;\n\
           GET DIAGNOSTICS y = ROW_COUNT;\n\
           SELECT 1, 2 INTO y, z; SELECT 3 AS f INTO r; r.f := 4;\n\
           BEGIN RAISE NOTICE 'x'; EXCEPTION WHEN OTHERS THEN y := 5; END;\n\
           RETURN y;\n\
         END; $$ LANGUAGE plpgsql;",
    )
    .unwrap();

    assert!(parsed.datums.len() >= 9);
    assert_eq!(parsed.action.body.len(), 9);
    assert!(parsed
        .datums
        .iter()
        .any(|datum| matches!(datum, PLpgSQLDatum::RecField { field, .. } if field == "f")));
    assert!(parsed.action.body.iter().any(|stmt| matches!(
        stmt,
        PLpgSQLStmt::GetDiagnostics { items }
            if items.as_slice() == [("ROW_COUNT".to_string(), 3)]
    )));
}

#[test]
fn omitted_zero_datum_references_remain_valid_but_malformed_values_fail() {
    let datums = vec![scalar_datum("target")];
    let assignment = serde_json::json!({
        "PLpgSQL_stmt_assign": { "expr": json_expr("target := 1", 3) }
    });
    assert!(matches!(
        lower_stmt(&assignment, &datums).unwrap(),
        PLpgSQLStmt::Assign { target: 0, .. }
    ));

    let diagnostics = serde_json::json!({
        "PLpgSQL_stmt_getdiag": {
            "diag_items": [{ "PLpgSQL_diag_item": { "kind": "ROW_COUNT" } }]
        }
    });
    assert!(matches!(
        lower_stmt(&diagnostics, &datums).unwrap(),
        PLpgSQLStmt::GetDiagnostics { items } if items == vec![("ROW_COUNT".into(), 0)]
    ));

    for bad in [
        serde_json::json!(-1),
        serde_json::json!("0"),
        serde_json::json!(1.5),
    ] {
        let malformed = serde_json::json!({
            "PLpgSQL_stmt_assign": {
                "varno": bad,
                "expr": json_expr("target := 1", 3),
            }
        });
        assert!(matches!(
            lower_stmt(&malformed, &datums),
            Err(SQLError::Internal(_))
        ));
    }
}

#[test]
fn malformed_datum_identity_type_and_cross_references_are_rejected() {
    let missing_name = serde_json::json!({
        "PLpgSQL_var": {
            "datatype": { "PLpgSQL_type": { "typname": "integer" } }
        }
    });
    assert!(
        matches!(lower_datum(&missing_name), Err(SQLError::Internal(message)) if message.contains("refname"))
    );

    let missing_type = serde_json::json!({ "PLpgSQL_var": { "refname": "x" } });
    assert!(
        matches!(lower_datum(&missing_type), Err(SQLError::Internal(message)) if message.contains("datatype"))
    );

    let wrong_parent = vec![
        scalar_datum("not_a_record"),
        PLpgSQLDatum::RecField {
            field: "f".into(),
            parent: 0,
        },
    ];
    assert!(
        matches!(validate_datums(&wrong_parent), Err(SQLError::Internal(message)) if message.contains("not a record"))
    );

    let missing_row_target = vec![PLpgSQLDatum::Row {
        fields: vec![PLpgSQLRowField {
            name: "x".into(),
            varno: 9,
        }],
    }];
    assert!(
        matches!(validate_datums(&missing_row_target), Err(SQLError::Internal(message)) if message.contains("missing datum 9"))
    );
}

#[test]
fn malformed_nested_statement_tags_and_lists_are_never_skipped() {
    let datums = vec![scalar_datum("target")];
    let cases = [
        serde_json::json!({
            "PLpgSQL_stmt_if": {
                "cond": json_expr("true", 2),
                "elsif_list": [{ "wrong_elsif_tag": {} }]
            }
        }),
        serde_json::json!({
            "PLpgSQL_stmt_case": {
                "case_when_list": [{ "wrong_case_tag": {} }]
            }
        }),
        serde_json::json!({
            "PLpgSQL_stmt_getdiag": {
                "diag_items": [{ "wrong_diagnostic_tag": {} }]
            }
        }),
    ];
    for malformed in cases {
        assert!(matches!(
            lower_stmt(&malformed, &datums),
            Err(SQLError::Internal(_))
        ));
    }

    assert!(matches!(
        lower_stmt_list(&serde_json::json!({ "not": "an array" }), &datums),
        Err(SQLError::Internal(message)) if message.contains("not an array")
    ));

    let malformed_exception = serde_json::json!({
        "body": [],
        "exceptions": {
            "PLpgSQL_exception_block": {
                "exc_list": [{ "wrong_exception_tag": {} }]
            }
        }
    });
    assert!(matches!(
        lower_block(&malformed_exception, &datums),
        Err(SQLError::Internal(message)) if message.contains("exception arm")
    ));

    let unknown_exception_condition = serde_json::json!({
        "body": [],
        "exceptions": {
            "PLpgSQL_exception_block": {
                "exc_list": [{
                    "PLpgSQL_exception": {
                        "conditions": [{
                            "PLpgSQL_condition": { "condname": "not_a_condition" }
                        }],
                        "action": []
                    }
                }]
            }
        }
    });
    assert!(matches!(
        lower_block(&unknown_exception_condition, &datums),
        Err(SQLError::Internal(message)) if message.contains("not_a_condition")
    ));

    let unknown_raise_condition = serde_json::json!({
        "PLpgSQL_stmt_raise": {
            "elog_level": 21,
            "condname": "not_a_condition"
        }
    });
    assert!(matches!(
        lower_stmt(&unknown_raise_condition, &datums),
        Err(SQLError::Internal(message)) if message.contains("not_a_condition")
    ));
}

#[test]
fn postgres_condition_table_preserves_full_and_duplicate_mappings() {
    assert_eq!(condition_sqlstate("serialization_failure"), Some("40001"));
    assert_eq!(condition_sqlstate("disk_full"), Some("53100"));
    assert_eq!(
        condition_sqlstates("modifying_sql_data_not_permitted").collect::<Vec<_>>(),
        vec!["2F002", "38002"]
    );
    assert_eq!(condition_sqlstate("not_a_condition"), None);
}

#[test]
fn malformed_into_diagnostics_and_expression_modes_fail_at_lowering() {
    let datums = vec![scalar_datum("target")];
    let missing_into_target = serde_json::json!({
        "PLpgSQL_stmt_execsql": {
            "into": true,
            "sqlstmt": json_expr("SELECT 1", 0),
        }
    });
    assert!(matches!(
        lower_stmt(&missing_into_target, &datums),
        Err(SQLError::Internal(message)) if message.contains("INTO but no target")
    ));

    let missing_kind = serde_json::json!({
        "PLpgSQL_stmt_getdiag": {
            "diag_items": [{ "PLpgSQL_diag_item": {} }]
        }
    });
    assert!(matches!(
        lower_stmt(&missing_kind, &datums),
        Err(SQLError::Internal(message)) if message.contains("kind")
    ));

    assert!(matches!(
        lower_expr(&json_expr("1", 0)),
        Err(SQLError::Internal(message)) if message.contains("parse mode 0")
    ));
    assert!(matches!(
        lower_full_statement(&json_expr("SELECT 1", 2)),
        Err(SQLError::Internal(message)) if message.contains("parse mode 2")
    ));
}

#[test]
fn strip_assignment_single_name() {
    assert_eq!(
        strip_assignment_target("total := a + b", 1).unwrap().trim(),
        "a + b"
    );
    assert_eq!(strip_assignment_target("x = 1", 1).unwrap().trim(), "1");
}

#[test]
fn strip_assignment_quoted_and_dotted() {
    assert_eq!(
        strip_assignment_target("\"my var\" := 7", 1)
            .unwrap()
            .trim(),
        "7"
    );
    assert_eq!(
        strip_assignment_target("rec.fld := rec.fld + 1", 2)
            .unwrap()
            .trim(),
        "rec.fld + 1"
    );
}

#[test]
fn array_element_assignment_is_unsupported() {
    assert!(matches!(
        strip_assignment_target("arr[1] := 2", 1),
        Err(SQLError::Unsupported(_))
    ));
}
