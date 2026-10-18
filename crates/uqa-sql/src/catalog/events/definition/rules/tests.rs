//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::events::definition::tests::fixtures::Catalog;

fn collated_catalog() -> Catalog {
    let Statement::CreateTable(mut table) = crate::compile(
        "CREATE TABLE items(c text COLLATE \"C\", p text COLLATE \"POSIX\", plain text, n integer)",
    )
    .unwrap()
    .remove(0) else {
        panic!("table")
    };
    table.columns[0].collation_oid = Some(950);
    table.columns[1].collation_oid = Some(951);
    Catalog {
        columns: Some(table.columns),
        ..Catalog::default()
    }
}

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
        collation_oid: None,
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
            &[crate::ast::ColumnDef::nullable("id", ColumnType::Integer)],
            RuleEvent::Update,
            &mut dependencies,
        )
        .unwrap();
    assert!(condition.any_node(&|node| matches!(node,
        Expr::TypedLiteral { value: Value::Int(7), ty, .. } if ty == "integer"
    )));
    assert!(!condition.any_node(&|node| matches!(node, Expr::Literal(Value::Str(_)))));
}

mod collations;
mod diagnostics;

#[test]
fn rule_conditions_reject_set_returning_calls_only_when_created() {
    let catalog = collated_catalog();
    let context = catalog.context();
    let columns = catalog.columns.as_ref().unwrap();
    for sql in [
        "CREATE RULE r AS ON INSERT TO items WHERE generate_series(1, 2) > NEW.n DO ALSO SELECT 1",
        "CREATE RULE r AS ON UPDATE TO items WHERE regexp_matches(NEW.c, 'a') IS NOT NULL DO INSTEAD NOTHING",
    ] {
        let Statement::CreateRule(rule) = crate::compile(sql).unwrap().remove(0) else {
            panic!("rule")
        };
        let mut condition = rule.condition.clone().unwrap();
        let error = context
            .validate_rule_condition(
                &mut condition,
                columns,
                rule.event,
                None,
                None,
                RelationLookupMode::Dynamic,
            )
            .unwrap_err();
        assert_eq!(error.sqlstate(), Some("0A000"), "{sql}");
        assert_eq!(
            error.to_string(),
            "set-returning functions are not allowed in WHERE",
            "{sql}"
        );
        // A condition stored by an earlier release keeps loading.
        let mut condition = rule.condition.clone().unwrap();
        context
            .validate_rule_condition(
                &mut condition,
                columns,
                rule.event,
                None,
                None,
                RelationLookupMode::Bound,
            )
            .unwrap_or_else(|error| panic!("{sql}: {error}"));
    }
}
