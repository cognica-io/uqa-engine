//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn regclass_cast_preserves_postgresql_type_identity() {
    let expression = ScalarExpr::Cast {
        expr: Box::new(ScalarExpr::Literal(Value::Str("items".into()))),
        ty: "pg_catalog.regclass".into(),
    };
    assert_eq!(
        scalar_type(&expression, &RowSchema::default(), &[]).unwrap(),
        Some(ColumnType::Regclass)
    );
}

#[test]
fn common_type_matches_postgresql_numeric_and_left_character_precedence() {
    assert_eq!(
        common_type(&ColumnType::SmallInteger, &ColumnType::BigInteger).unwrap(),
        ColumnType::BigInteger
    );
    assert_eq!(
        common_type(
            &ColumnType::Numeric {
                precision: None,
                scale: None,
            },
            &ColumnType::Real,
        )
        .unwrap(),
        ColumnType::Real
    );
    assert_eq!(
        common_type(&ColumnType::Varchar(Some(8)), &ColumnType::Text).unwrap(),
        ColumnType::Varchar(None)
    );
    assert_eq!(
        common_type(&ColumnType::Text, &ColumnType::Varchar(Some(8))).unwrap(),
        ColumnType::Text
    );
    assert_eq!(
        common_type(&ColumnType::Oid, &ColumnType::BigInteger).unwrap(),
        ColumnType::Oid
    );
    assert_eq!(
        common_type(&ColumnType::Integer, &ColumnType::Oid).unwrap(),
        ColumnType::Oid
    );
}

#[test]
fn equality_resolution_rejects_postgresql_undefined_operators() {
    for (left, right) in [
        (ColumnType::Boolean, ColumnType::Integer),
        (ColumnType::Json, ColumnType::Json),
        (
            ColumnType::Array(Box::new(ColumnType::Integer)),
            ColumnType::Array(Box::new(ColumnType::BigInteger)),
        ),
    ] {
        let error = equality_operand_type(&left, &right).unwrap_err();
        assert_eq!(error.sqlstate(), Some("42883"));
    }
}

#[test]
fn values_type_resolution_uses_declared_casts_instead_of_runtime_values() {
    let rows = vec![
        vec![ScalarExpr::Cast {
            expr: Box::new(ScalarExpr::Literal(Value::Int(1))),
            ty: "smallint".into(),
        }],
        vec![ScalarExpr::Cast {
            expr: Box::new(ScalarExpr::Literal(Value::Int(2))),
            ty: "bigint".into(),
        }],
    ];
    assert_eq!(
        values_column_types(&rows, &[]).unwrap(),
        vec![Some(ColumnType::BigInteger)]
    );
}

#[test]
fn type_introspection_binds_before_integer_width_is_erased() {
    let schema = RowSchema::with_types(vec!["v".into()], vec![Some(ColumnType::SmallInteger)]);
    let expression = ScalarExpr::Func {
        name: "pg_typeof".into(),
        binding: None,
        args: vec![ScalarExpr::Column("v".into())],
        distinct: false,
        order_by: Vec::new(),
        filter: None,
    };
    assert_eq!(
        bind_type_introspection(expression, &schema, &[]),
        ScalarExpr::Cast {
            expr: Box::new(ScalarExpr::Literal(Value::Str("smallint".into()))),
            ty: "regtype".into(),
        }
    );
}

#[test]
fn to_hex_binds_the_declared_integer_overload_before_width_is_erased() {
    let schema = RowSchema::with_types(
        vec!["i4".into(), "i8".into()],
        vec![Some(ColumnType::Integer), Some(ColumnType::BigInteger)],
    );
    for (column, expected_name) in [("i4", TO_HEX_INT4_FUNCTION), ("i8", TO_HEX_INT8_FUNCTION)] {
        let expression = ScalarExpr::Func {
            name: "to_hex".into(),
            binding: None,
            args: vec![ScalarExpr::Column(column.into())],
            distinct: false,
            order_by: Vec::new(),
            filter: None,
        };
        let ScalarExpr::Func { name, .. } = bind_type_introspection(expression, &schema, &[])
        else {
            panic!("to_hex must remain a scalar function");
        };
        assert_eq!(name, expected_name);
    }
}

#[test]
fn qualified_type_introspection_binds_inside_an_expression() {
    let schema = RowSchema::with_types(vec!["v".into()], vec![Some(ColumnType::Real)]);
    let expression = ScalarExpr::IsNull {
        expr: Box::new(ScalarExpr::Func {
            name: "PG_CATALOG.PG_TYPEOF".into(),
            binding: None,
            args: vec![ScalarExpr::Column("v".into())],
            distinct: false,
            order_by: Vec::new(),
            filter: None,
        }),
        negated: false,
    };
    assert_eq!(
        bind_type_introspection(expression, &schema, &[]),
        ScalarExpr::IsNull {
            expr: Box::new(ScalarExpr::Cast {
                expr: Box::new(ScalarExpr::Literal(Value::Str("real".into()))),
                ty: "regtype".into(),
            }),
            negated: false,
        }
    );
}

#[test]
fn type_binding_reuses_existing_expression_storage() {
    let expression = ScalarExpr::And(vec![ScalarExpr::Between {
        expr: Box::new(ScalarExpr::Column("v".into())),
        low: Box::new(ScalarExpr::Literal(Value::Int(1))),
        high: Box::new(ScalarExpr::Literal(Value::Int(9))),
    }]);
    let ScalarExpr::And(items) = &expression else {
        unreachable!();
    };
    let items_address = items.as_ptr();
    let ScalarExpr::Between { expr, .. } = &items[0] else {
        unreachable!();
    };
    let expression_address = std::ptr::from_ref::<ScalarExpr>(expr.as_ref());

    let bound = bind_type_introspection(expression, &RowSchema::default(), &[]);

    let ScalarExpr::And(items) = &bound else {
        panic!("bound expression must preserve the conjunction");
    };
    let ScalarExpr::Between { expr, .. } = &items[0] else {
        panic!("bound expression must preserve the range predicate");
    };
    assert_eq!(items.as_ptr(), items_address);
    assert_eq!(
        std::ptr::from_ref::<ScalarExpr>(expr.as_ref()),
        expression_address
    );
}

#[test]
fn array_cast_binding_preserves_the_declared_source_element_type() {
    let source = ScalarExpr::Array(vec![ScalarExpr::Cast {
        expr: Box::new(ScalarExpr::Literal(Value::Int(1))),
        ty: "smallint".into(),
    }]);
    let expression = ScalarExpr::Cast {
        expr: Box::new(source.clone()),
        ty: "bytea[]".into(),
    };
    assert_eq!(
        bind_type_introspection(expression, &RowSchema::default(), &[]),
        ScalarExpr::Cast {
            expr: Box::new(ScalarExpr::Cast {
                expr: Box::new(source),
                ty: "smallint[]".into(),
            }),
            ty: "bytea[]".into(),
        }
    );
}

#[test]
fn common_type_binding_coerces_selector_results_before_runtime_evaluation() {
    let schema = RowSchema::with_types(
        vec!["floating".into(), "exact".into()],
        vec![
            Some(ColumnType::DoublePrecision),
            Some(ColumnType::Numeric {
                precision: None,
                scale: None,
            }),
        ],
    );
    let expression = ScalarExpr::Func {
        name: "coalesce".into(),
        binding: None,
        args: vec![
            ScalarExpr::Column("floating".into()),
            ScalarExpr::Column("exact".into()),
        ],
        distinct: false,
        order_by: Vec::new(),
        filter: None,
    };

    let bound = bind_type_introspection(expression, &schema, &[]);
    let ScalarExpr::Func { args, .. } = &bound else {
        panic!("COALESCE must remain a function expression");
    };
    assert!(matches!(
        args.as_slice(),
        [
            ScalarExpr::Column(_),
            ScalarExpr::Cast { ty, .. }
        ] if ty == "double precision"
    ));
    assert_eq!(
        scalar_type(&bound, &schema, &[]).unwrap(),
        Some(ColumnType::DoublePrecision)
    );
}
