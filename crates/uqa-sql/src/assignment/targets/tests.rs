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

struct PairCatalog;

impl crate::expr::composites::CompositeTypeCatalog for PairCatalog {
    fn composite_type(
        &self,
        type_oid: u32,
    ) -> Result<Option<std::sync::Arc<crate::expr::composites::CompositeTypeDescriptor>>, SQLError>
    {
        Ok((type_oid == 30_002).then(|| {
            std::sync::Arc::new(crate::expr::composites::CompositeTypeDescriptor {
                dropped: Vec::new(),
                type_oid,
                relation_oid: 30_001,
                attributes: vec![
                    crate::expr::composites::CompositeAttribute {
                        name: "x".into(),
                        ty: ColumnType::Integer,
                        number: 1,
                    },
                    crate::expr::composites::CompositeAttribute {
                        name: "tags".into(),
                        ty: ColumnType::Array(Box::new(ColumnType::Text)),
                        number: 2,
                    },
                ],
            })
        }))
    }
}

fn pair_type() -> ColumnType {
    ColumnType::Composite(crate::ast::CompositeTypeReference {
        schema: "public".into(),
        name: "pair".into(),
        oid: 30_002,
        array_oid: 30_003,
        relation_oid: 30_001,
    })
}

#[test]
fn field_assignments_walk_composite_fields_and_subscript_groups() {
    let index = || Box::new(ScalarExpr::Literal(Value::Int(1)));
    let target = |indirection: Vec<AssignmentStep<ScalarExpr>>| AssignmentTarget {
        column: "p".into(),
        indirection,
    };
    let array_of_pairs = ColumnType::Array(Box::new(pair_type()));
    let element_field = target(vec![
        AssignmentStep::Index(index()),
        AssignmentStep::Field("tags".into()),
        AssignmentStep::Index(index()),
    ]);
    assert!(has_field_step(&element_field));
    assert_eq!(assignment_levels(&element_field.indirection).len(), 3);
    assert_eq!(
        field_assignment_types(&element_field, &array_of_pairs, Some(&PairCatalog))
            .unwrap()
            .last(),
        Some(&ColumnType::Text)
    );
    let missing = field_assignment_types(
        &target(vec![AssignmentStep::Field("missing".into())]),
        &pair_type(),
        Some(&PairCatalog),
    )
    .unwrap_err();
    assert_eq!(missing.sqlstate(), Some("42703"));
    assert_eq!(
        missing.to_string(),
        "cannot assign to field \"missing\" of column \"p\" because there is no such column in data type pair"
    );
    let scalar = field_assignment_types(
        &target(vec![
            AssignmentStep::Field("x".into()),
            AssignmentStep::Field("y".into()),
        ]),
        &pair_type(),
        Some(&PairCatalog),
    )
    .unwrap_err();
    assert_eq!(scalar.sqlstate(), Some("42804"));
    assert_eq!(
        scalar.to_string(),
        "cannot assign to field \"y\" of column \"p\" because its type integer is not a composite type"
    );
    let subscripted = field_assignment_types(
        &target(vec![
            AssignmentStep::Field("x".into()),
            AssignmentStep::Index(index()),
        ]),
        &pair_type(),
        Some(&PairCatalog),
    )
    .unwrap_err();
    assert_eq!(
        subscripted.to_string(),
        "cannot subscript type integer because it does not support subscripting"
    );
    let mismatch = validate_assignment_source(
        &target(vec![AssignmentStep::Field("x".into())]),
        &ColumnType::Integer,
        Some(&ColumnType::Boolean),
    )
    .unwrap_err();
    assert_eq!(
        mismatch.to_string(),
        "subfield \"x\" is of type integer but expression is of type boolean"
    );
}
