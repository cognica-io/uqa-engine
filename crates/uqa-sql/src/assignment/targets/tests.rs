//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Untyped destinations cannot erase assignment indirection.

use super::*;
use crate::ScalarExpr;
use uqa_core::Value;

#[test]
fn partial_assignment_requires_a_declared_container_type() {
    let index = Box::new(ScalarExpr::Literal(Value::Int(1)));
    for step in [
        AssignmentStep::Index(index.clone()),
        AssignmentStep::Slice {
            lower: None,
            upper: Some(index),
        },
    ] {
        let target = AssignmentTarget {
            column: "value".into(),
            indirection: vec![step],
        };
        let error = validate_assignment_type(&target, None).unwrap_err();
        assert_eq!(error.sqlstate(), Some("42804"));
        assert_eq!(
            error.to_string(),
            "cannot subscript type unknown because it does not support subscripting"
        );
        validate_assignment_type(
            &target,
            Some(&ColumnType::Array(Box::new(ColumnType::Integer))),
        )
        .unwrap();
    }
    validate_assignment_type(
        &AssignmentTarget::<ScalarExpr>::from("value".to_string()),
        None,
    )
    .unwrap();
}

#[test]
fn untyped_field_assignment_does_not_become_a_whole_column_write() {
    let target = AssignmentTarget::<ScalarExpr> {
        column: "value".into(),
        indirection: vec![AssignmentStep::Field("part".into())],
    };
    let error = validate_assignment_type(&target, None).unwrap_err();
    assert_eq!(error.sqlstate(), Some("42804"));
    assert_eq!(error.to_string(), "cannot assign to field \"part\" of column \"value\" because its type unknown is not a composite type");
}
