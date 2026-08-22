//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn int_node(value: i32) -> Node {
    Node {
        node: Some(NodeEnum::AConst(pg_query::protobuf::AConst {
            val: Some(pg_query::protobuf::a_const::Val::Ival(
                pg_query::protobuf::Integer { ival: value },
            )),
            ..Default::default()
        })),
    }
}

#[test]
fn set_operation_requires_both_children() {
    let missing_left = pg_query::protobuf::SelectStmt {
        op: pg_query::protobuf::SetOperation::SetopUnion as i32,
        rarg: Some(Box::default()),
        ..Default::default()
    };
    let error = compile_set_op(&missing_left).unwrap_err();
    assert!(matches!(
        error,
        SQLError::Internal(message) if message.contains("missing left")
    ));

    let missing_right = pg_query::protobuf::SelectStmt {
        op: pg_query::protobuf::SetOperation::SetopUnion as i32,
        larg: Some(Box::default()),
        ..Default::default()
    };
    let error = compile_set_op(&missing_right).unwrap_err();
    assert!(matches!(
        error,
        SQLError::Internal(message) if message.contains("missing right")
    ));
}

#[test]
fn malformed_scalar_nodes_never_fall_back_to_null_or_default_semantics() {
    let empty_constant = pg_query::protobuf::AConst::default();
    assert!(matches!(
        compile_const(&empty_constant),
        Err(SQLError::Internal(message)) if message.contains("no value payload")
    ));

    let zero_parameter = Node {
        node: Some(NodeEnum::ParamRef(pg_query::protobuf::ParamRef::default())),
    };
    assert!(matches!(
        compile_expr(&zero_parameter),
        Err(SQLError::Internal(message)) if message.contains("greater than zero")
    ));

    let invalid_null_test = pg_query::protobuf::NullTest {
        arg: Some(Box::new(int_node(1))),
        ..Default::default()
    };
    assert!(matches!(
        compile_null_test(&invalid_null_test),
        Err(SQLError::Internal(message)) if message.contains("invalid kind")
    ));

    let malformed_not = pg_query::protobuf::BoolExpr {
        boolop: pg_query::protobuf::BoolExprType::NotExpr as i32,
        args: vec![int_node(1), int_node(2)],
        ..Default::default()
    };
    assert!(matches!(
        compile_bool_expr(&malformed_not),
        Err(SQLError::Internal(message)) if message.contains("exactly one")
    ));
}

#[test]
fn malformed_sort_and_window_flags_are_rejected() {
    let undefined_sort = pg_query::protobuf::SortBy::default();
    assert!(matches!(
        compile_sort_options(&undefined_sort, "test ORDER BY"),
        Err(SQLError::Internal(message)) if message.contains("undefined sort direction")
    ));

    let negative_frame = pg_query::protobuf::WindowDef {
        frame_options: -1,
        ..Default::default()
    };
    assert!(matches!(
        compile_window_frame(&negative_frame),
        Err(SQLError::Internal(message)) if message.contains("cannot be negative")
    ));

    let exclusion_frame = pg_query::protobuf::WindowDef {
        frame_options: 0x000_0001 | 0x000_0004 | 0x000_0020 | 0x000_0400 | 0x000_8000,
        ..Default::default()
    };
    assert!(matches!(
        compile_window_frame(&exclusion_frame),
        Err(SQLError::Unsupported(message)) if message.contains("EXCLUDE")
    ));
}

#[test]
fn unsupported_expression_shapes_fail_instead_of_losing_semantics() {
    for (sql, expected) in [
        (
            "SELECT 2 > ANY (SELECT value FROM values_table)",
            "ANY subquery operator",
        ),
        (
            "SELECT count(*) FILTER (WHERE true) OVER ()",
            "aggregate modifiers",
        ),
        (
            "SELECT sum(value) OVER (ORDER BY value ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW EXCLUDE CURRENT ROW) FROM values_table",
            "EXCLUDE",
        ),
        ("SELECT 1 ORDER BY 1 USING >", "USING operators"),
    ] {
        let error = crate::compile(sql).expect_err(sql);
        assert!(
            matches!(&error, SQLError::Unsupported(message) if message.contains(expected)),
            "unexpected error for {sql}: {error}"
        );
    }
}

#[test]
fn type_modifiers_are_checked_without_numeric_truncation() {
    for sql in [
        "CREATE TABLE bad_vector (embedding vector(-1))",
        "CREATE TABLE zero_vector (embedding vector(0))",
        "CREATE TABLE extra_vector (embedding vector(2, 3))",
        "CREATE TABLE extra_numeric (amount numeric(10, 2, 1))",
    ] {
        assert!(crate::compile(sql).is_err(), "unexpected success for {sql}");
    }
}
