//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "covers polymorphic array binding cases"
)]
fn array_transforms_bind_polymorphic_types_named_slots_and_boolean_unknowns() {
    let schema = RowSchema::with_types(
        vec![
            "integers".into(),
            "documents".into(),
            "integer_domain".into(),
        ],
        vec![
            Some(ColumnType::Array(Box::new(ColumnType::Integer))),
            Some(ColumnType::Array(Box::new(ColumnType::Json))),
            Some(ColumnType::Domain {
                collation_oid: None,
                schema: "public".into(),
                name: "integer_array_domain".into(),
                oid: 99_999,
                array_oid: None,
                base: Box::new(ColumnType::Array(Box::new(ColumnType::Integer))),
            }),
        ],
    );
    let named = named_argument;
    let call = |name: &str, args| ScalarExpr::Func {
        order_syntax: crate::ast::FunctionOrderSyntax::Ordinary,
        name: name.into(),
        binding: None,
        args,
        distinct: false,
        order_by: Vec::new(),
        filter: None,
    };
    let expression = call(
        "array_sort",
        vec![
            named("descending", ScalarExpr::Literal(Value::Str("true".into()))),
            named("array", ScalarExpr::Column("integers".into())),
        ],
    );
    assert_eq!(
        scalar_type(&expression, &schema, &[]).unwrap(),
        Some(ColumnType::Array(Box::new(ColumnType::Integer)))
    );
    let ScalarExpr::Func { name, args, .. } = bind_type_introspection(expression, &schema, &[])
    else {
        panic!("array_sort must remain a scalar function");
    };
    assert_eq!(name, "array_sort");
    assert_eq!(args[0], ScalarExpr::Column("integers".into()));
    assert!(matches!(
        &args[1],
        ScalarExpr::Cast { ty, .. } if ty == &ColumnType::Boolean.catalog_name()
    ));

    let parameterized = call(
        "array_sort",
        vec![ScalarExpr::Column("integers".into()), ScalarExpr::Param(1)],
    );
    let parameters = [SQLParam::Scalar(Value::Str("true".into()))];
    assert_eq!(
        scalar_type(&parameterized, &schema, &parameters).unwrap(),
        Some(ColumnType::Array(Box::new(ColumnType::Integer)))
    );
    for parameter in [
        SQLParam::Scalar(Value::Bool(true)),
        SQLParam::Scalar(Value::Null),
        SQLParam::typed_scalar(Value::Bool(true), ColumnType::Boolean),
    ] {
        assert_eq!(
            scalar_type(&parameterized, &schema, &[parameter]).unwrap(),
            Some(ColumnType::Array(Box::new(ColumnType::Integer)))
        );
    }
    for parameter in [
        SQLParam::Scalar(Value::Int(1)),
        SQLParam::typed_scalar(Value::Str("true".into()), ColumnType::Text),
        SQLParam::typed_scalar(Value::Null, ColumnType::Text),
    ] {
        assert_eq!(
            scalar_type(&parameterized, &schema, &[parameter])
                .unwrap_err()
                .sqlstate(),
            Some("42883")
        );
    }
    let ScalarExpr::Func { args, .. } =
        bind_type_introspection(parameterized, &schema, &parameters)
    else {
        panic!("parameterized array_sort must remain a scalar function");
    };
    assert!(matches!(
        &args[1],
        ScalarExpr::Cast { expr, ty, .. }
            if ty == &ColumnType::Boolean.catalog_name() && matches!(expr.as_ref(), ScalarExpr::Param(1))
    ));

    let json_sort = call("array_sort", vec![ScalarExpr::Column("documents".into())]);
    let ScalarExpr::Func { name, binding, .. } = bind_type_introspection(json_sort, &schema, &[])
    else {
        panic!("json array_sort must remain a scalar function");
    };
    assert_eq!(name, "array_sort");
    assert_eq!(
        binding.and_then(|binding| binding.dispatch),
        Some(FunctionDispatch::ArraySortJson)
    );

    let domain_sort = call(
        "array_sort",
        vec![ScalarExpr::Column("integer_domain".into())],
    );
    assert_eq!(
        scalar_type(&domain_sort, &schema, &[]).unwrap(),
        Some(ColumnType::Array(Box::new(ColumnType::Integer)))
    );

    let unknown = call("array_reverse", vec![ScalarExpr::Literal(Value::Null)]);
    let error = scalar_type(&unknown, &schema, &[]).unwrap_err();
    assert_eq!(error.sqlstate(), Some("42804"));
    assert_eq!(
        error.to_string(),
        "could not determine polymorphic type because input has type unknown"
    );
    let invalid = call(
        "array_sort",
        vec![
            ScalarExpr::Column("integers".into()),
            ScalarExpr::Literal(Value::Int(1)),
        ],
    );
    let error = scalar_type(&invalid, &schema, &[]).unwrap_err();
    assert_eq!(error.sqlstate(), Some("42883"));
    assert_eq!(
        error.to_string(),
        "function array_sort(integer[], integer) does not exist"
    );
}
