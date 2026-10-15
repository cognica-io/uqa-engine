//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::schema::constraints::{check_references, CheckReference};
use std::collections::BTreeSet;

#[test]
fn check_reference_order_and_automatic_names_match_postgresql() {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("catalog_references.json")).unwrap();
    assert_eq!(fixture["cases"].as_array().unwrap().len(), 3);
    for case in fixture["cases"].as_array().unwrap() {
        let mut table = super::lifecycle::analyzed(case["sql"].as_str().unwrap());
        let mut names = BTreeSet::new();
        let mut actual = Vec::new();
        for check in &mut table.checks {
            crate::schema::constraint_metadata::assign_check_name(
                &table.name,
                &check.expr,
                &mut check.name,
                &mut names,
            )
            .unwrap();
            let numbers = check_references(&check.expr)
                .into_iter()
                .map(|reference| {
                    reference
                        .attribute_number(|name| {
                            let (position, column) = table
                                .columns
                                .iter()
                                .enumerate()
                                .find(|(_, column)| column.name == name)
                                .unwrap();
                            crate::catalog::relation_attributes::column_number(column, position)
                        })
                        .unwrap()
                        .to_string()
                })
                .collect::<Vec<_>>();
            let key = (!numbers.is_empty()).then(|| format!("{{{}}}", numbers.join(",")));
            actual.push(serde_json::json!([check.name, key]));
        }
        actual.sort_by(|left, right| left[0].as_str().cmp(&right[0].as_str()));
        assert_eq!(serde_json::json!(actual), case["keys"], "{}", case["sql"]);
    }
}

#[test]
fn check_references_borrow_names_and_keep_only_the_first_variable_position() {
    let expression = Expr::And(vec![
        Expr::QualifiedStar("scoped".into()),
        Expr::Column("b".into()),
        Expr::Column("tableoid".into()),
        Expr::QualifiedColumn {
            qualifier: "scoped".into(),
            column: "b".into(),
        },
        Expr::QualifiedStar("scoped".into()),
    ]);
    let Expr::And(items) = &expression else {
        unreachable!();
    };
    let Expr::Column(original) = &items[1] else {
        unreachable!();
    };
    let references = check_references(&expression);
    assert_eq!(
        references,
        [
            CheckReference::WholeRow,
            CheckReference::Column("b"),
            CheckReference::Column("tableoid"),
        ]
    );
    assert!(std::ptr::eq(
        references[1].column_name().unwrap(),
        original.as_str()
    ));
    assert_eq!(
        references[1].attribute_number(|_| Ok(7)).unwrap(),
        7,
        "live columns keep durable attribute numbers after dropped slots"
    );
    for (reference, expected) in [(references[0], 0), (references[2], -6)] {
        assert_eq!(
            reference
                .attribute_number(|_| panic!("not an ordinary attribute"))
                .unwrap(),
            expected
        );
    }
}
