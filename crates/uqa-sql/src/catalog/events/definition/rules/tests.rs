//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::events::definition::tests::fixtures::Catalog;

#[test]
fn ordinary_rule_conditions_retain_analyzed_constants_without_composite_casts() {
    let Statement::CreateRule(definition) =
        crate::compile("CREATE RULE selected AS ON UPDATE TO items WHERE NEW.id = '7' DO NOTHING")
            .unwrap()
            .remove(0)
    else {
        panic!("rule");
    };
    let mut condition = definition.condition.unwrap();
    let mut plan = crate::plan::ExpressionPlan::lower(condition.clone());
    let crate::ScalarExpr::Binary { rhs, .. } = &mut plan.scalar else {
        panic!("comparison");
    };
    **rhs = crate::plan::ExpressionPlan::lower(Expr::TypedLiteral {
        value: Value::Int(7),
        ty: "integer".into(),
        composite_source: None,
    })
    .scalar;
    let catalog = Catalog::default();
    let mut dependencies = RuleDependencies::default();
    catalog
        .context()
        .bind_rule_condition_object_dependencies(
            &mut condition,
            Some(&plan),
            &[("id".into(), ColumnType::Integer)],
            RuleEvent::Update,
            &mut dependencies,
        )
        .unwrap();
    assert!(condition.any_node(&|node| matches!(node,
        Expr::TypedLiteral { value: Value::Int(7), ty, .. } if ty == "integer"
    )));
    assert!(!condition.any_node(&|node| matches!(node, Expr::Literal(Value::Str(_)))));
}
