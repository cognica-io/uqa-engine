//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bound function syntax returns to the exact stored expression site.

use super::{bind_stored_expression_sites, bind_stored_statement_sites};
use crate::ast::{Expr, FunctionOrderSyntax, Statement};
use crate::binding::syntax_sites::{expression_syntax_sites, query_syntax_sites};
use crate::catalog::stored_ast::{visit_stored_expression, visit_stored_statement_expressions};
use crate::plan::{ExpressionPlan, QueryPlan};
use crate::{SQLError, ScalarExpr};
use uqa_core::Value;

fn expression(sql: &str) -> Expr {
    let Statement::Select(mut statement) =
        crate::compile(&format!("SELECT {sql}")).unwrap().remove(0)
    else {
        panic!("SELECT expected");
    };
    statement.projections.remove(0).expr
}

fn legacy(node: &mut Expr) -> Result<(), SQLError> {
    if let Expr::Func { order_syntax, .. } = node {
        *order_syntax = FunctionOrderSyntax::Legacy;
    }
    Ok(())
}

fn resolved_order(node: &mut ScalarExpr) {
    if let ScalarExpr::Func {
        name, order_syntax, ..
    } = node
    {
        *order_syntax = if matches!(name.as_str(), "mode" | "percentile_disc") {
            FunctionOrderSyntax::WithinGroup
        } else {
            FunctionOrderSyntax::Ordinary
        };
    }
}

#[test]
fn composite_constructor_sites_preserve_children_and_rebinding_is_idempotent() {
    for sql in [
        "ROW(1, lower('A'))::pair",
        "ROW(1, lower('A'))::pair_domain",
    ] {
        let mut syntax = expression(sql);
        let original = ExpressionPlan::lower(syntax.clone());
        let ScalarExpr::Cast { expr, .. } = &original.scalar else {
            panic!("cast")
        };
        let ScalarExpr::Row(items) = expr.as_ref() else {
            panic!("row")
        };
        let row = ScalarExpr::CompositeRow {
            bound_type: None,
            items: items.clone(),
            binding: crate::ast::CompositeRowBinding {
                ty: "composite#20001".into(),
                attributes: vec![1, 3],
            },
        };
        let mut bound = original.clone();
        bound.scalar = if sql.ends_with("pair_domain") {
            ScalarExpr::Cast {
                implicit: false,
                expr: Box::new(row),
                ty: "domain#20002".into(),
            }
        } else {
            row
        };
        let sites = expression_syntax_sites(&original, &bound).unwrap();
        assert!(bind_stored_expression_sites(&mut syntax, &sites).unwrap());
        let restored = ExpressionPlan::lower(syntax.clone());
        assert_eq!(restored.scalar, bound.scalar);
        let sites = expression_syntax_sites(&restored, &restored).unwrap();
        assert!(!bind_stored_expression_sites(&mut syntax, &sites).unwrap());
        let encoded = serde_json::to_string(&syntax).unwrap();
        assert_eq!(serde_json::from_str::<Expr>(&encoded).unwrap(), syntax);
    }
}

#[test]
fn stored_expression_receives_function_order_and_typed_inputs_at_their_own_sites() {
    let mut syntax = expression(
        "coalesce(percentile_disc('0.5') WITHIN GROUP (ORDER BY 'ordered'::text), 'fallback')",
    );
    visit_stored_expression(&mut syntax, &mut legacy).unwrap();
    let original = ExpressionPlan::lower(syntax.clone());
    let mut bound = original.clone();
    bound.scalar.visit_mut(&mut |node| {
        resolved_order(node);
        if matches!(node, ScalarExpr::Literal(Value::Str(text)) if text == "0.5") {
            *node = ScalarExpr::TypedLiteral {
                value: Value::Float(0.5),
                ty: "double precision".into(),
                bound_type: None,
                parameter_index: None,
            };
        }
    });
    let sites = expression_syntax_sites(&original, &bound).unwrap();
    assert!(bind_stored_expression_sites(&mut syntax, &sites).unwrap());
    let restored = ExpressionPlan::lower(syntax.clone());
    assert_eq!(
        serde_json::to_value(&restored).unwrap(),
        serde_json::to_value(&bound).unwrap()
    );
    let sites = expression_syntax_sites(&restored, &restored).unwrap();
    assert!(!bind_stored_expression_sites(&mut syntax, &sites).unwrap());
    let encoded = serde_json::to_string(&syntax).unwrap();
    assert!(encoded.contains("\"order_syntax\":\"WithinGroup\""));
    assert_eq!(serde_json::from_str::<Expr>(&encoded).unwrap(), syntax);
}

#[test]
fn stored_query_order_sites_follow_ctes_and_scalar_subqueries() {
    let mut syntax = crate::compile(
        "WITH input AS (SELECT mode() WITHIN GROUP (ORDER BY lower('X')) AS value) SELECT value, (SELECT array_agg(1 ORDER BY 1)) FROM input",
    )
    .unwrap()
    .remove(0);
    visit_stored_statement_expressions(&mut syntax, &mut legacy).unwrap();
    let Statement::Select(statement) = &syntax else {
        panic!("SELECT expected");
    };
    let original = QueryPlan::lower(statement.as_ref().clone());
    let mut bound = original.clone();
    bound.rewrite_scalar_expressions(&mut resolved_order);
    let sites = query_syntax_sites(&original, &bound).unwrap();
    assert!(bind_stored_statement_sites(&mut syntax, &sites).unwrap());
    let Statement::Select(statement) = &syntax else {
        panic!("SELECT expected");
    };
    let restored = QueryPlan::lower(statement.as_ref().clone());
    assert_eq!(
        serde_json::to_value(&restored).unwrap(),
        serde_json::to_value(&bound).unwrap()
    );
    let sites = query_syntax_sites(&restored, &restored).unwrap();
    assert!(!bind_stored_statement_sites(&mut syntax, &sites).unwrap());
}

#[test]
fn function_order_sites_do_not_reinterpret_explicit_syntax() {
    let syntax = expression("f(a ORDER BY b)");
    let original = ExpressionPlan::lower(syntax.clone());
    let mut bound = original.clone();
    let ScalarExpr::Func { order_syntax, .. } = &mut bound.scalar else {
        panic!("function expected");
    };
    *order_syntax = FunctionOrderSyntax::WithinGroup;
    assert!(expression_syntax_sites(&original, &bound)
        .unwrap_err()
        .to_string()
        .contains("function ordering"));

    let mut legacy_syntax = syntax.clone();
    visit_stored_expression(&mut legacy_syntax, &mut legacy).unwrap();
    let legacy_plan = ExpressionPlan::lower(legacy_syntax);
    let sites = expression_syntax_sites(&legacy_plan, &bound).unwrap();
    let mut explicit_syntax = syntax;
    assert!(bind_stored_expression_sites(&mut explicit_syntax, &sites)
        .unwrap_err()
        .to_string()
        .contains("function ordering"));
}

#[test]
fn unresolved_legacy_function_order_is_preserved_without_serialized_metadata() {
    let mut syntax = expression("f(a ORDER BY b)");
    visit_stored_expression(&mut syntax, &mut legacy).unwrap();
    let lowered = ExpressionPlan::lower(syntax.clone());
    let sites = expression_syntax_sites(&lowered, &lowered).unwrap();
    assert!(!bind_stored_expression_sites(&mut syntax, &sites).unwrap());
    assert!(!serde_json::to_string(&syntax)
        .unwrap()
        .contains("order_syntax"));
}

#[test]
fn canonical_window_input_sites_preserve_each_stored_copy_without_raw_orphan_queries() {
    let mut syntax = crate::compile("SELECT sum(v) OVER w, row_number() OVER w FROM t WINDOW w AS (ORDER BY (SELECT '7'::integer)), unused AS (ORDER BY '9'::integer)").unwrap().remove(0);
    let Statement::Select(statement) = &syntax else {
        panic!("SELECT expected")
    };
    let original = QueryPlan::lower(statement.as_ref().clone());
    let crate::plan::RelationalPlan::QueryBlock(block) = &original.root else {
        panic!("query block")
    };
    assert_eq!(block.subqueries.len(), 1);
    let mut bound = original.clone();
    bound.rewrite_scalar_expressions(&mut |node| {
        if let ScalarExpr::Literal(Value::Str(text)) = node {
            if let Ok(value) = text.parse::<i64>() {
                *node = ScalarExpr::TypedLiteral {
                    value: Value::Int(value),
                    ty: "integer".into(),
                    bound_type: None,
                    parameter_index: None,
                };
            }
        }
    });
    bound.normalize_window_definitions().unwrap();
    let sites = query_syntax_sites(&original, &bound).unwrap();
    assert!(bind_stored_statement_sites(&mut syntax, &sites).unwrap());
    let Statement::Select(statement) = &syntax else {
        panic!("SELECT expected")
    };
    let restored = QueryPlan::lower(statement.as_ref().clone());
    assert_eq!(
        serde_json::to_value(&restored).unwrap(),
        serde_json::to_value(&bound).unwrap()
    );
    let sites = query_syntax_sites(&restored, &restored).unwrap();
    assert!(!bind_stored_statement_sites(&mut syntax, &sites).unwrap());
}

#[test]
fn statement_sites_preserve_common_coercions_across_storage_and_relowering() {
    let mut syntax = crate::compile(
        "WITH input AS (SELECT coalesce(ARRAY[s],ARRAY[b]) AS value) SELECT value, (SELECT coalesce(ARRAY[s],ARRAY[b])) FROM input",
    ).unwrap().remove(0);
    let Statement::Select(statement) = &syntax else {
        panic!("SELECT expected")
    };
    let original = QueryPlan::lower(statement.as_ref().clone());
    let schema = crate::RowSchema::with_types(
        vec!["s".into(), "b".into()],
        vec![
            Some(crate::ColumnType::SmallInteger),
            Some(crate::ColumnType::BigInteger),
        ],
    );
    let mut bound = original.clone();
    bound.rewrite_scalar_expressions(&mut |node| {
        if matches!(node, ScalarExpr::Func { name, .. } if name == "coalesce") {
            *node = crate::type_resolution::bind_type_introspection(node.clone(), &schema, &[]);
        }
    });
    let sites = query_syntax_sites(&original, &bound).unwrap();
    assert!(bind_stored_statement_sites(&mut syntax, &sites).unwrap());
    let serialized = serde_json::to_string(&syntax).unwrap();
    assert_eq!(serialized.matches(r#""implicit":true"#).count(), 2);
    let mut restored: Statement = serde_json::from_str(&serialized).unwrap();
    let Statement::Select(statement) = &restored else {
        panic!("SELECT expected")
    };
    let lowered = QueryPlan::lower(statement.as_ref().clone());
    assert_eq!(
        serde_json::to_value(&lowered).unwrap(),
        serde_json::to_value(&bound).unwrap()
    );
    let sites = query_syntax_sites(&lowered, &lowered).unwrap();
    assert!(!bind_stored_statement_sites(&mut restored, &sites).unwrap());
}

#[test]
fn stored_set_operations_bind_each_left_input_once() {
    let mut syntax = crate::compile("SELECT f('left') UNION ALL SELECT f('right')")
        .unwrap()
        .remove(0);
    let Statement::Select(statement) = &syntax else {
        panic!("SELECT expected")
    };
    let original = QueryPlan::lower(statement.as_ref().clone());
    let sites = query_syntax_sites(&original, &original).unwrap();
    assert!(!bind_stored_statement_sites(&mut syntax, &sites).unwrap());
    let mut calls = 0;
    visit_stored_statement_expressions(&mut syntax, &mut |node| {
        calls += usize::from(matches!(node, Expr::Func { .. }));
        Ok(())
    })
    .unwrap();
    assert_eq!(calls, 2);
    let Statement::Select(statement) = syntax else {
        panic!("SELECT expected")
    };
    assert_eq!(
        serde_json::to_value(QueryPlan::lower(*statement)).unwrap(),
        serde_json::to_value(original).unwrap()
    );
}
