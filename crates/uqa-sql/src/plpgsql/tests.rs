//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::lowering_statement::{lower_stmt, lower_stmt_list};
use super::parsing::{
    lower_datum, lower_function, parse_plpgsql_text, synthesize_create_text, validate_datums,
};
use super::*;

#[test]
fn plpgsql_synthesized_definition_preserves_variadic_parameters() {
    let mut statements = crate::compile(
        "CREATE FUNCTION variadic_probe(VARIADIC items integer[]) RETURNS integer LANGUAGE plpgsql AS $$ BEGIN RETURN cardinality(items); END $$",
    )
    .unwrap();
    let Statement::CreateFunction(definition) = statements.remove(0) else {
        panic!("expected CREATE FUNCTION");
    };
    let FunctionBody::Source(body) = &definition.body else {
        panic!("expected source body");
    };
    let synthesized = synthesize_create_text(&definition, body);
    assert!(synthesized.contains("VARIADIC \"items\" int4[]"));
    parse_function(&definition).unwrap();
}

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
    assert_eq!(definition.scroll, None);
    assert!(matches!(definition.query, Statement::Select(_)));

    let PLpgSQLStmt::OpenCursor {
        cursor,
        open: PLpgSQLCursorOpen::Bound { arguments },
    } = &parsed.action.body[0]
    else {
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
            direction: CursorDirection::Forward,
            count: PLpgSQLCursorCount::Constant(1),
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
fn pg18_dynamic_and_bound_cursor_for_loops_lower_structurally() {
    let parsed = parse_plpgsql_text(
        "CREATE FUNCTION row_loops() RETURNS integer LANGUAGE plpgsql AS $$
         DECLARE a integer; b text; cursor_row text := 'outer';
                 c CURSOR (low_value integer, high_value integer)
                   FOR SELECT low_value AS value WHERE low_value <= high_value;
         BEGIN
           <<dynamic_rows>>
           FOR a, b IN EXECUTE 'SELECT $1, $2' USING 1, 'x' LOOP NULL; END LOOP dynamic_rows;
           <<cursor_rows>>
           FOR cursor_row IN c(high_value => 2, low_value => 1) LOOP
             a := cursor_row.value;
           END LOOP cursor_rows;
           RETURN a;
         END
         $$;",
    )
    .unwrap();

    let cursor = parsed
        .datums
        .iter()
        .position(|datum| matches!(datum, PLpgSQLDatum::Var(var) if var.name == "c"))
        .unwrap();
    let cursor_target = parsed
        .datums
        .iter()
        .position(|datum| matches!(datum, PLpgSQLDatum::Rec { name } if name == "cursor_row"))
        .unwrap();
    let PLpgSQLStmt::ForDynamic {
        label,
        target: IntoTarget::Row(fields),
        params,
        body,
        ..
    } = &parsed.action.body[0]
    else {
        panic!("expected dynamic-query FOR as the first statement");
    };
    assert_eq!(label.as_deref(), Some("dynamic_rows"));
    assert_eq!(
        fields
            .iter()
            .map(|field| field.name.as_str())
            .collect::<Vec<_>>(),
        vec!["a", "b"]
    );
    assert_eq!(params.len(), 2);
    assert!(body.is_empty());

    let PLpgSQLStmt::ForCursor {
        label,
        target,
        cursor: lowered_cursor,
        arguments,
        body,
    } = &parsed.action.body[1]
    else {
        panic!("expected bound-cursor FOR as the second statement");
    };
    assert_eq!(label.as_deref(), Some("cursor_rows"));
    assert_eq!(*target, cursor_target);
    assert_eq!(*lowered_cursor, cursor);
    assert_eq!(
        arguments
            .iter()
            .map(|argument| argument.name.as_deref())
            .collect::<Vec<_>>(),
        vec![Some("low_value"), Some("high_value")]
    );
    assert_eq!(body.len(), 1);
    assert_eq!(
        parsed.loop_local_variable_datums(),
        std::collections::BTreeSet::from([cursor_target])
    );
}

#[test]
fn pg18_dynamic_open_fetch_directions_and_move_lower_structurally() {
    let parsed = parse_plpgsql_text(
        "CREATE FUNCTION cursor_controls() RETURNS integer LANGUAGE plpgsql AS $$
         DECLARE c refcursor; value integer;
         BEGIN
           OPEN c SCROLL FOR SELECT x FROM generate_series(1, 3) AS g(x);
           FETCH LAST FROM c INTO value;
           MOVE FORWARD 1 + 1 FROM c;
           CLOSE c;
           OPEN c NO SCROLL FOR EXECUTE 'SELECT $1' USING 7;
           FETCH RELATIVE -1 FROM c INTO value;
           CLOSE c;
           RETURN value;
         END
         $$;",
    )
    .unwrap();

    assert!(matches!(
        &parsed.action.body[0],
        PLpgSQLStmt::OpenCursor {
            open: PLpgSQLCursorOpen::Static {
                scroll: Some(true),
                ..
            },
            ..
        }
    ));
    assert!(matches!(
        &parsed.action.body[1],
        PLpgSQLStmt::FetchCursor {
            direction: CursorDirection::Absolute,
            count: PLpgSQLCursorCount::Constant(-1),
            ..
        }
    ));
    assert!(matches!(
        &parsed.action.body[2],
        PLpgSQLStmt::MoveCursor {
            direction: CursorDirection::Forward,
            count: PLpgSQLCursorCount::Expression(_),
            ..
        }
    ));
    assert!(matches!(
        &parsed.action.body[4],
        PLpgSQLStmt::OpenCursor {
            open: PLpgSQLCursorOpen::Dynamic {
                params,
                scroll: Some(false),
                ..
            },
            ..
        } if params.len() == 1
    ));
    assert!(matches!(
        &parsed.action.body[5],
        PLpgSQLStmt::FetchCursor {
            direction: CursorDirection::Relative,
            count: PLpgSQLCursorCount::Expression(_),
            ..
        }
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
            PLpgSQLDatum::Var(variable)
                if matches!(variable.cursor.as_ref(), Some(cursor) if cursor.argument_row.is_none())
        )
    }));
}

#[test]
fn pg18_percent_type_identifiers_lower_as_structured_identity() {
    let parsed = parse_plpgsql_text(
        "CREATE FUNCTION quoted_type_reference() RETURNS void AS $$ DECLARE local_value \"app.dot\".\"typed.dot\".\"id.dot\"%TYPE; BEGIN RETURN; END $$ LANGUAGE plpgsql;",
    )
    .unwrap();
    let PLpgSQLDatum::Var(variable) = parsed
        .datums
        .iter()
        .find(
            |datum| matches!(datum, PLpgSQLDatum::Var(variable) if variable.name == "local_value"),
        )
        .unwrap()
    else {
        unreachable!();
    };
    assert_eq!(
        variable.type_reference,
        Some(RoutineColumnTypeReference::new(
            Some("app.dot".into()),
            "typed.dot".into(),
            "id.dot".into(),
        ))
    );
}

#[test]
fn pg18_builtin_array_datums_use_sql_array_spelling() {
    let parsed = parse_plpgsql_text(
        "CREATE FUNCTION array_datum(vals integer[]) RETURNS integer[] AS $$ DECLARE local_vals integer[]; BEGIN RETURN vals; END $$ LANGUAGE plpgsql;",
    )
    .unwrap();
    let array_types = parsed
        .datums
        .iter()
        .filter_map(|datum| match datum {
            PLpgSQLDatum::Var(variable) if variable.type_name.ends_with("[]") => {
                Some(variable.type_name.as_str())
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(array_types.len() >= 2, "array datums: {array_types:?}");
    assert!(array_types.iter().all(|type_name| *type_name == "int4[]"));
}

#[test]
fn trigger_datum_indices_must_reference_existing_datums() {
    for field in ["new_varno", "old_varno"] {
        let mut function = serde_json::json!({
            "datums": [],
            "action": { "PLpgSQL_stmt_block": { "body": [] } }
        });
        function[field] = serde_json::json!(0);
        let error = lower_function(&function).expect_err("out-of-range trigger datum must fail");
        assert!(
            matches!(error, SQLError::Internal(ref message) if message.contains(field) && message.contains("out-of-range")),
            "{error}"
        );
    }
}

fn scalar_datum(name: &str) -> PLpgSQLDatum {
    PLpgSQLDatum::Var(Box::new(PLpgSQLVar {
        name: name.into(),
        type_name: "integer".into(),
        type_reference: None,
        default: None,
        constant: false,
        not_null: false,
        cursor: None,
        lineno: None,
    }))
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
fn pg18_foreach_array_statements_preserve_targets_slices_labels_and_bodies() {
    let parsed = parse_plpgsql_text(
        "CREATE FUNCTION foreach_shape(items integer[]) RETURNS text LANGUAGE plpgsql AS $$\n\
         DECLARE item integer; piece integer[]; out_text text := '';\n\
         BEGIN\n\
           FOREACH item IN ARRAY items LOOP out_text := out_text || item; END LOOP;\n\
           <<slice_loop>> FOREACH piece SLICE 1 IN ARRAY items LOOP EXIT slice_loop; END LOOP slice_loop;\n\
           RETURN out_text;\n\
         END $$;",
    )
    .unwrap();

    let loops = parsed
        .action
        .body
        .iter()
        .filter_map(|statement| match statement {
            PLpgSQLStmt::ForeachArray {
                label,
                target,
                slice,
                body,
                ..
            } => Some((label.as_deref(), *target, *slice, body)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(loops.len(), 2);
    assert_eq!(loops[0].0, None);
    assert_eq!(loops[0].2, 0);
    assert_eq!(parsed.datums[loops[0].1].name(), Some("item"));
    assert_eq!(loops[0].3.len(), 1);
    assert_eq!(loops[1].0, Some("slice_loop"));
    assert_eq!(loops[1].2, 1);
    assert_eq!(parsed.datums[loops[1].1].name(), Some("piece"));
    assert!(matches!(
        loops[1].3.as_slice(),
        [PLpgSQLStmt::Exit {
            is_exit: true,
            label: Some(label),
            cond: None,
        }] if label == "slice_loop"
    ));
}

#[test]
fn pg18_assert_statements_preserve_conditions_and_lazy_messages() {
    let parsed = parse_plpgsql_text(
        "CREATE FUNCTION assert_shape(flag boolean) RETURNS integer LANGUAGE plpgsql AS $$
         BEGIN
           ASSERT flag;
           ASSERT flag, 'flag=' || flag;
           RETURN 1;
         END $$;",
    )
    .unwrap();

    assert!(matches!(
        parsed.action.body[0],
        PLpgSQLStmt::Assert { message: None, .. }
    ));
    assert!(matches!(
        parsed.action.body[1],
        PLpgSQLStmt::Assert {
            message: Some(_),
            ..
        }
    ));
}

#[test]
fn pg18_procedural_transaction_statements_preserve_chain_mode() {
    let parsed = parse_plpgsql_text(
        "CREATE PROCEDURE transaction_shape() LANGUAGE plpgsql AS $$
         BEGIN
           COMMIT;
           COMMIT AND CHAIN;
           COMMIT AND NO CHAIN;
           ROLLBACK;
           ROLLBACK AND CHAIN;
           ROLLBACK AND NO CHAIN;
         END $$;",
    )
    .unwrap();

    let modes = parsed
        .action
        .body
        .iter()
        .filter_map(|statement| match statement {
            PLpgSQLStmt::Commit { chain } => Some(("commit", *chain)),
            PLpgSQLStmt::Rollback { chain } => Some(("rollback", *chain)),
            PLpgSQLStmt::Return { value: None } => None,
            other => panic!("unexpected transaction statement: {other:?}"),
        })
        .collect::<Vec<_>>();
    assert_eq!(
        modes,
        [
            ("commit", false),
            ("commit", true),
            ("commit", false),
            ("rollback", false),
            ("rollback", true),
            ("rollback", false),
        ]
    );
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

    let foreach = serde_json::json!({
        "PLpgSQL_stmt_foreach_a": {
            "expr": json_expr("ARRAY[1]", 2),
            "body": []
        }
    });
    assert!(matches!(
        lower_stmt(&foreach, &datums).unwrap(),
        PLpgSQLStmt::ForeachArray {
            target: 0,
            slice: 0,
            ..
        }
    ));

    let missing_foreach_target = serde_json::json!({
        "PLpgSQL_stmt_foreach_a": {
            "varno": 1,
            "expr": json_expr("ARRAY[1]", 2),
            "body": []
        }
    });
    assert!(matches!(
        lower_stmt(&missing_foreach_target, &datums),
        Err(SQLError::Internal(message)) if message.contains("missing datum 1")
    ));

    let negative_foreach_slice = serde_json::json!({
        "PLpgSQL_stmt_foreach_a": {
            "slice": -1,
            "expr": json_expr("ARRAY[1]", 2),
            "body": []
        }
    });
    assert!(matches!(
        lower_stmt(&negative_foreach_slice, &datums),
        Err(SQLError::Internal(message)) if message.contains("slice")
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
fn malformed_assert_nodes_fail_at_lowering() {
    let missing_condition = serde_json::json!({
        "PLpgSQL_stmt_assert": {
            "message": json_expr("'message'", 2)
        }
    });
    assert!(matches!(
        lower_stmt(&missing_condition, &[]),
        Err(SQLError::Internal(message)) if message.contains("cond")
    ));

    let malformed_message = serde_json::json!({
        "PLpgSQL_stmt_assert": {
            "cond": json_expr("true", 2),
            "message": []
        }
    });
    assert!(matches!(
        lower_stmt(&malformed_message, &[]),
        Err(SQLError::Internal(_))
    ));
}
