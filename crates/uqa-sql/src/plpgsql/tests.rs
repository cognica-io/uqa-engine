//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::lowering_statement::{lower_stmt, lower_stmt_list};
use super::parsing::{lower_datum, parse_plpgsql_text, validate_datums};
use super::*;

#[test]
fn pg18_bound_cursor_named_arguments_lower_in_declaration_order() {
    let parsed = parse_plpgsql_text(
        "CREATE FUNCTION cursor_probe() RETURNS integer LANGUAGE plpgsql AS $$ DECLARE c CURSOR (a integer, b integer) FOR SELECT a + b AS value; out_value integer; BEGIN OPEN c(b => 2, a => 1); FETCH c INTO out_value; CLOSE c; RETURN out_value; END $$;",
    )
    .unwrap();

    let cursor_index = parsed
        .datums
        .iter()
        .position(|datum| matches!(datum, PLpgSQLDatum::Var(var) if var.name == "c"))
        .unwrap();
    let PLpgSQLDatum::Var(cursor) = &parsed.datums[cursor_index] else {
        unreachable!();
    };
    let definition = cursor.cursor.as_ref().expect("bound cursor definition");
    assert!(definition.argument_row.is_some());
    assert!(matches!(definition.query, Statement::Select(_)));

    let PLpgSQLStmt::OpenCursor { cursor, arguments } = &parsed.action.body[0] else {
        panic!("expected OPEN as the first statement");
    };
    assert_eq!(*cursor, cursor_index);
    assert_eq!(
        arguments
            .iter()
            .map(|argument| argument.name.as_deref())
            .collect::<Vec<_>>(),
        vec![Some("a"), Some("b")]
    );
    assert!(matches!(arguments[0].expr, Expr::Literal(Value::Int(1))));
    assert!(matches!(arguments[1].expr, Expr::Literal(Value::Int(2))));
    assert!(matches!(
        parsed.action.body[1],
        PLpgSQLStmt::FetchCursor {
            cursor,
            direction: 0,
            count: 1,
            ..
        } if cursor == cursor_index
    ));
    assert!(matches!(
        parsed.action.body[2],
        PLpgSQLStmt::CloseCursor { cursor } if cursor == cursor_index
    ));
    assert!(matches!(
        parsed.action.body[3],
        PLpgSQLStmt::Return {
            value: Some(PLpgSQLReturnValue::Datum(index))
        } if index == 5
    ));
}

#[test]
fn pg18_return_slots_and_cursor_minus_one_sentinel_lower_directly() {
    let scalar = parse_plpgsql_text(
        "CREATE FUNCTION return_slot(x integer) RETURNS integer AS $$ BEGIN RETURN x; END $$ LANGUAGE plpgsql;",
    )
    .unwrap();
    assert!(matches!(
        scalar.action.body[0],
        PLpgSQLStmt::Return {
            value: Some(PLpgSQLReturnValue::Datum(0))
        }
    ));

    let set = parse_plpgsql_text(
        "CREATE FUNCTION return_next_slot(x integer) RETURNS SETOF integer AS $$ BEGIN RETURN NEXT x; RETURN; END $$ LANGUAGE plpgsql;",
    )
    .unwrap();
    assert!(matches!(
        set.action.body[0],
        PLpgSQLStmt::ReturnNext {
            value: Some(PLpgSQLReturnValue::Datum(0))
        }
    ));

    let cursor = parse_plpgsql_text(
        "CREATE FUNCTION cursor_no_args() RETURNS integer AS $$ DECLARE c CURSOR FOR SELECT 1 AS value; out_value integer; BEGIN OPEN c; FETCH c INTO out_value; CLOSE c; RETURN out_value; END $$ LANGUAGE plpgsql;",
    )
    .unwrap();
    assert!(cursor.datums.iter().any(|datum| {
        matches!(
            datum,
            PLpgSQLDatum::Var(PLpgSQLVar {
                cursor: Some(PLpgSQLCursor {
                    argument_row: None,
                    ..
                }),
                ..
            })
        )
    }));
}

fn scalar_datum(name: &str) -> PLpgSQLDatum {
    PLpgSQLDatum::Var(PLpgSQLVar {
        name: name.into(),
        type_name: "integer".into(),
        default: None,
        constant: false,
        not_null: false,
        cursor: None,
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
