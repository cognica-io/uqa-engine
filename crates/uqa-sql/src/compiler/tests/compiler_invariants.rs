//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Compiler-wide parser, administrative-command, and malformed-AST invariants.

use super::*;
use crate::ast::{VacuumOption, VacuumOptionValue, VacuumTarget};

#[test]
fn analyze_preserves_its_relation_and_rejects_dropped_semantics() {
    let Statement::Analyze { table } = first("ANALYZE app.docs") else {
        panic!("not ANALYZE");
    };
    assert_eq!(table.as_deref(), Some("app.docs"));
    let Statement::Analyze { table } = first("ANALYZE") else {
        panic!("not ANALYZE");
    };
    assert!(table.is_none());

    for (sql, expected) in [
        ("ANALYZE docs (title)", "column lists"),
        ("ANALYZE (VERBOSE) docs", "options"),
    ] {
        let error = compile(sql).expect_err(sql);
        assert!(
            matches!(&error, SQLError::Unsupported(message) if message.contains(expected)),
            "unexpected error for {sql}: {error}"
        );
    }
    let Statement::Vacuum(vacuum) = first("VACUUM (ANALYZE, PARALLEL 2) ONLY app.docs (title)")
    else {
        panic!("not VACUUM");
    };
    assert_eq!(
        vacuum.options,
        vec![
            VacuumOption {
                name: "analyze".into(),
                value: None,
            },
            VacuumOption {
                name: "parallel".into(),
                value: Some(VacuumOptionValue::Integer(2)),
            },
        ]
    );
    assert_eq!(
        vacuum.targets,
        vec![VacuumTarget {
            catalog: None,
            table: "app.docs".into(),
            include_descendants: false,
            columns: vec!["title".into()],
        }]
    );
}

#[test]
fn malformed_type_cast_never_degrades_to_the_uncast_expression() {
    let cast = Node {
        node: Some(NodeEnum::TypeCast(Box::new(pg_query::protobuf::TypeCast {
            arg: Some(Box::new(null_literal_node())),
            type_name: None,
            ..Default::default()
        }))),
    };

    let error = compile_expr(&cast).unwrap_err();
    assert!(error.to_string().contains("without a target type"));
}

#[test]
fn malformed_operator_name_is_not_silently_discarded() {
    let expression = Node {
        node: Some(NodeEnum::AExpr(Box::new(pg_query::protobuf::AExpr {
            kind: pg_query::protobuf::AExprKind::AexprOp as i32,
            name: vec![Node::default()],
            lexpr: Some(Box::new(null_literal_node())),
            rexpr: Some(Box::new(null_literal_node())),
            ..Default::default()
        }))),
    };

    let error = compile_expr(&expression).unwrap_err();
    assert!(error.to_string().contains("missing string node"));
}

#[test]
fn prefix_minus_preserves_the_cast_operand() {
    let Statement::Select(select) = first("SELECT -1::smallint") else {
        panic!("expected SELECT");
    };
    let [projection] = select.projections.as_slice() else {
        panic!("expected one projection");
    };
    assert!(matches!(
        &projection.expr,
        Expr::UnaryMinus(inner)
            if matches!(inner.as_ref(), Expr::Cast { ty, .. } if ty == "smallint")
    ));
}
