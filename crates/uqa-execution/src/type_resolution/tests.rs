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
fn builtin_argument_targets_resolve_fixed_and_overloaded_unknowns() {
    assert_eq!(
        builtin_function_argument_targets("PG_CATALOG.BTRIM", &[None]),
        vec![Some(ColumnType::Text)]
    );
    assert_eq!(
        builtin_function_argument_targets(
            "concat_op",
            &[None, Some(ColumnType::Array(Box::new(ColumnType::Integer)))],
        ),
        vec![
            Some(ColumnType::Array(Box::new(ColumnType::Integer))),
            Some(ColumnType::Array(Box::new(ColumnType::Integer))),
        ]
    );
    assert_eq!(
        builtin_function_argument_targets("uuid_extract_version", &[None]),
        vec![Some(ColumnType::Uuid)]
    );
    assert_eq!(
        builtin_function_argument_targets("random", &[None, Some(ColumnType::BigInteger)]),
        vec![Some(ColumnType::BigInteger), Some(ColumnType::BigInteger)]
    );
}

#[test]
fn uuid_extraction_binding_rejects_non_uuid_declared_types() {
    let schema = RowSchema::with_types(
        vec!["uuid_value".into(), "text_value".into()],
        vec![Some(ColumnType::Uuid), Some(ColumnType::Text)],
    );
    let call = |column: &str| ScalarExpr::Func {
        name: "uuid_extract_version".into(),
        binding: None,
        args: vec![ScalarExpr::Column(column.into())],
        distinct: false,
        order_by: Vec::new(),
        filter: None,
    };

    let ScalarExpr::Func { name, .. } = bind_type_introspection(call("uuid_value"), &schema, &[])
    else {
        panic!("UUID extraction must remain a function call");
    };
    assert_eq!(name, "uuid_extract_version");

    let ScalarExpr::Func { name, .. } = bind_type_introspection(call("text_value"), &schema, &[])
    else {
        panic!("an unresolved UUID overload must remain an error marker call");
    };
    assert_eq!(
        name,
        format!(
            "{}uuid_extract_version(text)",
            uqa_sql::expr::UNDEFINED_FUNCTION_MARKER
        )
    );
}

#[test]
fn uuid_extraction_binding_uses_declared_scalar_subquery_types() {
    struct SubqueryTypes;

    impl FunctionTypeResolver for SubqueryTypes {
        fn resolve_function_type(
            &self,
            _name: &str,
            _binding: Option<&FunctionBinding>,
            _argument_names: &[Option<String>],
            _argument_types: &[Option<ColumnType>],
        ) -> Result<Option<ColumnType>, SQLError> {
            Ok(None)
        }

        fn resolve_scalar_subquery_type(
            &self,
            subquery: crate::SubqueryId,
            _outer_schema: &RowSchema,
            _params: &[SQLParam],
        ) -> Result<Option<ColumnType>, SQLError> {
            Ok(Some(if subquery == 0 {
                ColumnType::Uuid
            } else {
                ColumnType::Text
            }))
        }
    }

    let call = |subquery| ScalarExpr::Func {
        name: "uuid_extract_version".into(),
        binding: None,
        args: vec![ScalarExpr::ScalarSubquery(subquery)],
        distinct: false,
        order_by: Vec::new(),
        filter: None,
    };
    let bind = |subquery| {
        bind_type_introspection_with_resolver(
            call(subquery),
            &RowSchema::default(),
            &[],
            &SubqueryTypes,
        )
    };

    let ScalarExpr::Func { name, .. } = bind(0) else {
        panic!("UUID scalar subquery must remain a function call");
    };
    assert_eq!(name, "uuid_extract_version");
    let ScalarExpr::Func { name, .. } = bind(1) else {
        panic!("text scalar subquery must remain an error marker call");
    };
    assert_eq!(
        name,
        format!(
            "{}uuid_extract_version(text)",
            uqa_sql::expr::UNDEFINED_FUNCTION_MARKER
        )
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
fn integer_base_functions_bind_the_declared_overload_before_width_is_erased() {
    let schema = RowSchema::with_types(
        vec!["i4".into(), "i8".into()],
        vec![Some(ColumnType::Integer), Some(ColumnType::BigInteger)],
    );
    for (function, column, expected_name) in [
        ("to_bin", "i4", TO_BIN_INT4_FUNCTION),
        ("to_bin", "i8", TO_BIN_INT8_FUNCTION),
        ("to_hex", "i4", TO_HEX_INT4_FUNCTION),
        ("to_hex", "i8", TO_HEX_INT8_FUNCTION),
        ("to_oct", "i4", TO_OCT_INT4_FUNCTION),
        ("to_oct", "i8", TO_OCT_INT8_FUNCTION),
    ] {
        let expression = ScalarExpr::Func {
            name: function.into(),
            binding: None,
            args: vec![ScalarExpr::Column(column.into())],
            distinct: false,
            order_by: Vec::new(),
            filter: None,
        };
        let ScalarExpr::Func { name, .. } = bind_type_introspection(expression, &schema, &[])
        else {
            panic!("{function} must remain a scalar function");
        };
        assert_eq!(name, expected_name);
    }
}

#[test]
fn random_range_functions_bind_the_promoted_overload_before_width_is_erased() {
    let numeric = ColumnType::Numeric {
        precision: None,
        scale: None,
    };
    let schema = RowSchema::with_types(
        vec!["i2".into(), "i4".into(), "i8".into(), "n".into()],
        vec![
            Some(ColumnType::SmallInteger),
            Some(ColumnType::Integer),
            Some(ColumnType::BigInteger),
            Some(numeric),
        ],
    );
    for (lower, upper, expected_name) in [
        ("i4", "i4", RANDOM_INT4_FUNCTION),
        ("i2", "i4", RANDOM_INT4_FUNCTION),
        ("i4", "i8", RANDOM_INT8_FUNCTION),
        ("i8", "n", RANDOM_NUMERIC_FUNCTION),
    ] {
        let expression = ScalarExpr::Func {
            name: "random".into(),
            binding: None,
            args: vec![
                ScalarExpr::Column(lower.into()),
                ScalarExpr::Column(upper.into()),
            ],
            distinct: false,
            order_by: Vec::new(),
            filter: None,
        };
        let ScalarExpr::Func { name, .. } = bind_type_introspection(expression, &schema, &[])
        else {
            panic!("random range must remain a scalar function");
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

#[test]
fn nested_builtin_type_resolution_visits_each_argument_once() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct CountingResolver(AtomicUsize);

    impl FunctionTypeResolver for CountingResolver {
        fn resolve_function_type(
            &self,
            _name: &str,
            _binding: Option<&FunctionBinding>,
            _argument_names: &[Option<String>],
            _argument_types: &[Option<ColumnType>],
        ) -> Result<Option<ColumnType>, SQLError> {
            self.0.fetch_add(1, Ordering::Relaxed);
            Ok(Some(ColumnType::Integer))
        }
    }

    let mut expression = ScalarExpr::Func {
        name: "application.identity".into(),
        binding: None,
        args: vec![ScalarExpr::Literal(Value::Int(1))],
        distinct: false,
        order_by: Vec::new(),
        filter: None,
    };
    for _ in 0..16 {
        expression = ScalarExpr::Func {
            name: "round".into(),
            binding: None,
            args: vec![expression],
            distinct: false,
            order_by: Vec::new(),
            filter: None,
        };
    }
    let resolver = CountingResolver(AtomicUsize::new(0));

    assert_eq!(
        scalar_type_with_resolver(&expression, &RowSchema::default(), &[], &resolver).unwrap(),
        Some(ColumnType::DoublePrecision)
    );
    assert_eq!(resolver.0.load(Ordering::Relaxed), 1);
}

#[test]
fn null_only_transcendental_calls_resolve_to_double_precision() {
    for function in ["sqrt", "ln", "log", "log10"] {
        let expression = ScalarExpr::Func {
            name: function.into(),
            binding: None,
            args: vec![ScalarExpr::Literal(Value::Null)],
            distinct: false,
            order_by: Vec::new(),
            filter: None,
        };
        assert_eq!(
            scalar_type(&expression, &RowSchema::default(), &[]).unwrap(),
            Some(ColumnType::DoublePrecision),
            "{function}"
        );
    }
}
