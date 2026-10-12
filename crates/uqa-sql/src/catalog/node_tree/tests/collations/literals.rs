//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::ast::Expr;
use crate::catalog::node_tree::Field;
use uqa_core::Value;

#[test]
fn retained_constant_nodes_encode_selected_implicit_collations() {
    let schema = RowSchema::default();
    let context = expressions::ExpressionContext {
        schema: &schema,
        domain_value: None,
        types: None,
        routines: &Routines,
    };
    for collation_oid in [None, Some(0), Some(100), Some(950), Some(951)] {
        let literal = Expr::TypedLiteral {
            value: Value::Str("x".into()),
            ty: ColumnType::Text.catalog_name(),
            collation_oid,
            composite_source: None,
        };
        let node = context
            .check(&Expr::IsNull {
                expr: Box::new(literal),
                negated: false,
            })
            .unwrap();
        let Field::Node(argument) = node.field("arg").unwrap() else {
            panic!("NULLTEST argument")
        };
        assert_eq!(argument.kind, "CONST");
        assert_eq!(
            argument.integer("constcollid").unwrap(),
            i64::from(collation_oid.unwrap_or(100))
        );
    }
    let invalid = Expr::TypedLiteral {
        value: Value::Int(1),
        ty: ColumnType::Integer.catalog_name(),
        collation_oid: Some(950),
        composite_source: None,
    };
    let error = context
        .check(&Expr::IsNull {
            expr: Box::new(invalid),
            negated: false,
        })
        .unwrap_err();
    assert!(error
        .to_string()
        .contains("constant collation does not match"));
}
