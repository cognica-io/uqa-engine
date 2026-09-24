//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::ast::{ConstraintCatalogIdentity, TableKeyConstraintKind};
use uqa_core::Value;

fn declared(name: &str) -> EnforcedKey {
    TableKeyConstraint {
        catalog_identity: Some(ConstraintCatalogIdentity {
            object_id: [1; 16],
            oid: 50001,
        }),
        name: Some(name.into()),
        kind: TableKeyConstraintKind::Unique,
        columns: vec!["value".into()],
        nulls_not_distinct: true,
        without_overlaps: false,
    }
    .into()
}

#[test]
fn references_exclude_partial_and_expression_indexes_and_preserve_selected_metadata() {
    let ordinary = declared("ordinary");
    let expected = ordinary.constraint.clone();
    let mut partial = declared("partial");
    partial.predicate = Some(Box::new(Expr::Literal(Value::Bool(true))));
    let mut expression = declared("expression");
    expression.keys = vec![IndexKey::Expression(Box::new(Expr::Literal(Value::Int(1))))];
    let mut mixed = declared("mixed");
    mixed.keys.extend(expression.keys.clone());
    assert_eq!(
        referenceable_keys(vec![partial, ordinary, expression, mixed]),
        vec![expected]
    );
}
