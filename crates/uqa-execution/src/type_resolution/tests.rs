//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

mod fixed_overloads;

#[test]
fn typed_scalar_parameters_preserve_declared_width_domain_and_text_identity() {
    let domain = ColumnType::Domain {
        schema: "public".into(),
        name: "positive_int".into(),
        oid: 90_001,
        base: Box::new(ColumnType::Integer),
    };
    for (ty, value) in [
        (ColumnType::SmallInteger, Value::Int(7)),
        (domain.clone(), Value::Int(7)),
        (ColumnType::Text, Value::Str("seven".into())),
    ] {
        let parameters = [SQLParam::typed_scalar(value, ty.clone())];
        let expression = ScalarExpr::Param(1);
        let resolved = scalar_type(&expression, &RowSchema::default(), &parameters).unwrap();
        assert_eq!(resolved, Some(ty.clone()));
        assert_eq!(
            effective_overload_argument_type_with_params(&expression, resolved, &parameters),
            Some(ty)
        );
    }

    let scalar = [SQLParam::scalar(Value::Str("seven".into()))];
    let expression = ScalarExpr::Param(1);
    let resolved = scalar_type(&expression, &RowSchema::default(), &scalar).unwrap();
    assert_eq!(resolved, Some(ColumnType::Text));
    assert_eq!(
        effective_overload_argument_type_with_params(&expression, resolved, &scalar),
        None
    );
}

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
    assert_eq!(
        builtin_function_argument_targets("md5", &[None]),
        vec![Some(ColumnType::Text)]
    );
    assert_eq!(
        builtin_function_argument_targets("pg_catalog.md5", &[Some(ColumnType::Bytea)]),
        vec![Some(ColumnType::Bytea)]
    );
    for name in ["crc32", "crc32c", "pg_catalog.crc32"] {
        assert_eq!(
            builtin_function_argument_targets(name, &[None]),
            vec![Some(ColumnType::Bytea)],
            "{name}"
        );
    }
    for name in [
        "length",
        "char_length",
        "character_length",
        "octet_length",
        "bit_length",
    ] {
        assert_eq!(
            builtin_function_argument_targets(name, &[None]),
            vec![Some(ColumnType::Text)],
            "{name}"
        );
    }
    for name in ["length", "octet_length", "bit_length"] {
        assert_eq!(
            builtin_function_argument_targets(name, &[Some(ColumnType::Bytea)]),
            vec![Some(ColumnType::Bytea)],
            "{name}"
        );
    }
    for name in ["length", "char_length", "character_length", "octet_length"] {
        assert_eq!(
            builtin_function_argument_targets(name, &[Some(ColumnType::Character(3))]),
            vec![Some(ColumnType::Bpchar)],
            "{name}"
        );
    }
    assert_eq!(
        builtin_function_argument_targets("bit_length", &[Some(ColumnType::Character(3))]),
        vec![Some(ColumnType::Text)]
    );
    assert_eq!(
        builtin_function_argument_targets("json_strip_nulls", &[None, None]),
        vec![Some(ColumnType::Json), Some(ColumnType::Boolean)]
    );
    assert_eq!(
        builtin_function_argument_targets("pg_catalog.jsonb_strip_nulls", &[None]),
        vec![Some(ColumnType::JsonB)]
    );
}

#[test]
fn gamma_binding_preserves_the_float8_signature_and_function_identity() {
    let resolved =
        resolve_gamma_overload("gamma", None, &[None], &[Some(ColumnType::Integer)], None).unwrap();
    assert_eq!(resolved.return_type, ColumnType::DoublePrecision);
    assert_eq!(resolved.binding.name, "pg_catalog.gamma");
    assert_eq!(resolved.binding.argument_types, ["double precision"]);
    assert!(resolved.binding.builtin);

    let wrong_function = FunctionBinding {
        name: "pg_catalog.lgamma".into(),
        argument_types: vec!["double precision".into()],
        builtin: true,
        invocation: None,
    };
    let error = resolve_gamma_overload(
        "gamma",
        Some(&wrong_function),
        &[None],
        &[Some(ColumnType::DoublePrecision)],
        None,
    )
    .unwrap_err();
    assert_eq!(error.sqlstate(), Some("42883"));
}

#[test]
fn json_strip_binding_preserves_defaults_named_slots_and_declared_types() {
    let resolved = resolve_json_strip_overload(
        "json_strip_nulls",
        None,
        &[None],
        &[Some(ColumnType::Json)],
        None,
    )
    .unwrap();
    assert_eq!(resolved.return_type, ColumnType::Json);
    assert_eq!(resolved.binding.name, "pg_catalog.json_strip_nulls");
    assert_eq!(resolved.binding.argument_types, ["json", "boolean"]);
    assert!(resolved.binding.builtin);
    let json_domain = ColumnType::Domain {
        schema: "public".into(),
        name: "json_document".into(),
        oid: 99_997,
        base: Box::new(ColumnType::Json),
    };
    let boolean_domain = ColumnType::Domain {
        schema: "public".into(),
        name: "boolean_flag".into(),
        oid: 99_996,
        base: Box::new(ColumnType::Boolean),
    };
    assert_eq!(
        resolve_json_strip_overload(
            "json_strip_nulls",
            None,
            &[None, None],
            &[Some(json_domain), Some(boolean_domain)],
            None,
        )
        .unwrap()
        .return_type,
        ColumnType::Json
    );

    let named = |name: &str, value: ScalarExpr| ScalarExpr::Func {
        name: uqa_sql::expr::NAMED_ARG_FUNCTION.into(),
        binding: None,
        args: vec![ScalarExpr::Literal(Value::Str(name.into())), value],
        distinct: false,
        order_by: Vec::new(),
        filter: None,
    };
    let expression = ScalarExpr::Func {
        name: "jsonb_strip_nulls".into(),
        binding: None,
        args: vec![
            named("strip_in_arrays", ScalarExpr::Param(1)),
            named(
                "target",
                ScalarExpr::Literal(Value::Str(r#"{"a":null}"#.into())),
            ),
        ],
        distinct: false,
        order_by: Vec::new(),
        filter: None,
    };
    let parameters = [SQLParam::Scalar(Value::Str("true".into()))];
    assert_eq!(
        scalar_type(&expression, &RowSchema::default(), &parameters).unwrap(),
        Some(ColumnType::JsonB)
    );
    let ScalarExpr::Func {
        binding: Some(binding),
        args,
        ..
    } = bind_type_introspection(expression, &RowSchema::default(), &parameters)
    else {
        panic!("jsonb_strip_nulls must retain a bound scalar call");
    };
    assert_eq!(binding.name, "pg_catalog.jsonb_strip_nulls");
    assert_eq!(binding.argument_types, ["jsonb", "boolean"]);
    assert!(matches!(
        &args[0],
        ScalarExpr::Cast { expr, ty }
            if ty == "jsonb" && matches!(expr.as_ref(), ScalarExpr::Literal(Value::Str(_)))
    ));
    assert!(matches!(
        &args[1],
        ScalarExpr::Cast { expr, ty }
            if ty == "boolean" && matches!(expr.as_ref(), ScalarExpr::Param(1))
    ));

    let defaulted = ScalarExpr::Func {
        name: "json_strip_nulls".into(),
        binding: None,
        args: vec![ScalarExpr::Literal(Value::Str("{}".into()))],
        distinct: false,
        order_by: Vec::new(),
        filter: None,
    };
    let ScalarExpr::Func { args, .. } =
        bind_type_introspection(defaulted, &RowSchema::default(), &[])
    else {
        panic!("json_strip_nulls must retain a scalar call");
    };
    assert_eq!(args.len(), 2);
    assert_eq!(args[1], ScalarExpr::Literal(Value::Bool(false)));

    let invalid = ScalarExpr::Func {
        name: "json_strip_nulls".into(),
        binding: None,
        args: vec![ScalarExpr::Cast {
            expr: Box::new(ScalarExpr::Literal(Value::Str("{}".into()))),
            ty: "text".into(),
        }],
        distinct: false,
        order_by: Vec::new(),
        filter: None,
    };
    assert_eq!(
        scalar_type(&invalid, &RowSchema::default(), &[])
            .unwrap_err()
            .sqlstate(),
        Some("42883")
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
            _explicit_variadic: bool,
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
fn foreign_key_type_resolution_uses_the_referenced_operator_family() {
    let numeric = ColumnType::Numeric {
        precision: None,
        scale: None,
    };
    assert_eq!(
        foreign_key_operand_type(&ColumnType::Integer, &numeric).unwrap(),
        numeric
    );
    assert!(foreign_key_operand_type(&numeric, &ColumnType::Integer).is_err());
    assert_eq!(
        foreign_key_operand_type(&numeric, &ColumnType::Real).unwrap(),
        ColumnType::Real
    );
    assert!(foreign_key_operand_type(&ColumnType::Real, &numeric).is_err());
    assert_eq!(
        foreign_key_operand_type(&ColumnType::BigInteger, &ColumnType::SmallInteger).unwrap(),
        ColumnType::BigInteger
    );
    assert_eq!(
        foreign_key_operand_type(&ColumnType::Timestamp, &ColumnType::Date).unwrap(),
        ColumnType::Timestamp
    );
    assert_eq!(
        foreign_key_operand_type(&ColumnType::Text, &ColumnType::Character(8)).unwrap(),
        ColumnType::Bpchar
    );
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

    let named_max = ScalarExpr::Func {
        name: uqa_sql::expr::NAMED_ARG_FUNCTION.into(),
        binding: None,
        args: vec![
            ScalarExpr::Literal(Value::Str("max".into())),
            ScalarExpr::Column("i4".into()),
        ],
        distinct: false,
        order_by: Vec::new(),
        filter: None,
    };
    let invalid_order = ScalarExpr::Func {
        name: "random".into(),
        binding: None,
        args: vec![named_max, ScalarExpr::Column("i4".into())],
        distinct: false,
        order_by: Vec::new(),
        filter: None,
    };
    let error = scalar_type(&invalid_order, &schema, &[]).unwrap_err();
    assert_eq!(error.sqlstate(), Some("42601"));
}

#[test]
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
                schema: "public".into(),
                name: "integer_array_domain".into(),
                oid: 99_999,
                base: Box::new(ColumnType::Array(Box::new(ColumnType::Integer))),
            }),
        ],
    );
    let named = |name: &str, value: ScalarExpr| ScalarExpr::Func {
        name: uqa_sql::expr::NAMED_ARG_FUNCTION.into(),
        binding: None,
        args: vec![ScalarExpr::Literal(Value::Str(name.into())), value],
        distinct: false,
        order_by: Vec::new(),
        filter: None,
    };
    let call = |name: &str, args| ScalarExpr::Func {
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
        ScalarExpr::Cast { ty, .. } if ty == "boolean"
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
    let ScalarExpr::Func { args, .. } =
        bind_type_introspection(parameterized, &schema, &parameters)
    else {
        panic!("parameterized array_sort must remain a scalar function");
    };
    assert!(matches!(
        &args[1],
        ScalarExpr::Cast { expr, ty }
            if ty == "boolean" && matches!(expr.as_ref(), ScalarExpr::Param(1))
    ));

    let json_sort = call("array_sort", vec![ScalarExpr::Column("documents".into())]);
    let ScalarExpr::Func { name, .. } = bind_type_introspection(json_sort, &schema, &[]) else {
        panic!("json array_sort must remain a scalar function");
    };
    assert_eq!(name, uqa_sql::expr::ARRAY_SORT_JSON_FUNCTION);

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
fn text_cast_binding_preserves_only_legacy_vector_identity() {
    let schema = RowSchema::with_types(
        vec!["domain_value".into(), "arguments".into()],
        vec![
            Some(ColumnType::Domain {
                schema: "public".into(),
                name: "text_domain".into(),
                oid: 99_998,
                base: Box::new(ColumnType::Text),
            }),
            Some(ColumnType::OidVector),
        ],
    );
    let text_cast = |column: &str| ScalarExpr::Cast {
        expr: Box::new(ScalarExpr::Column(column.into())),
        ty: "text".into(),
    };
    assert_eq!(
        bind_type_introspection(text_cast("domain_value"), &schema, &[]),
        text_cast("domain_value")
    );
    assert_eq!(
        bind_type_introspection(text_cast("arguments"), &schema, &[]),
        ScalarExpr::Cast {
            expr: Box::new(ScalarExpr::Cast {
                expr: Box::new(ScalarExpr::Column("arguments".into())),
                ty: "oidvector".into(),
            }),
            ty: "text".into(),
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
            _explicit_variadic: bool,
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
fn explicit_variadic_marker_reaches_catalog_type_resolver() {
    struct VariadicResolver;

    impl FunctionTypeResolver for VariadicResolver {
        fn resolve_function_type(
            &self,
            name: &str,
            binding: Option<&FunctionBinding>,
            argument_names: &[Option<String>],
            argument_types: &[Option<ColumnType>],
            explicit_variadic: bool,
        ) -> Result<Option<ColumnType>, SQLError> {
            assert_eq!(name, "application.collect");
            assert_eq!(binding, None);
            assert_eq!(argument_names, [Some("items".into())]);
            assert_eq!(
                argument_types,
                [Some(ColumnType::Array(Box::new(ColumnType::Integer)))]
            );
            assert!(explicit_variadic);
            Ok(Some(ColumnType::Text))
        }
    }

    let array = ScalarExpr::Array(vec![ScalarExpr::Literal(Value::Int(1))]);
    let variadic = ScalarExpr::Func {
        name: uqa_sql::expr::VARIADIC_ARG_FUNCTION.into(),
        binding: None,
        args: vec![array],
        distinct: false,
        order_by: Vec::new(),
        filter: None,
    };
    let named = ScalarExpr::Func {
        name: uqa_sql::expr::NAMED_ARG_FUNCTION.into(),
        binding: None,
        args: vec![ScalarExpr::Literal(Value::Str("items".into())), variadic],
        distinct: false,
        order_by: Vec::new(),
        filter: None,
    };
    let expression = ScalarExpr::Func {
        name: "application.collect".into(),
        binding: None,
        args: vec![named],
        distinct: false,
        order_by: Vec::new(),
        filter: None,
    };

    assert_eq!(
        scalar_type_with_resolver(&expression, &RowSchema::default(), &[], &VariadicResolver,)
            .unwrap(),
        Some(ColumnType::Text)
    );
}

#[test]
fn explicit_variadic_marker_is_transparent_and_validates_call_position() {
    let marker = |value| ScalarExpr::Func {
        name: uqa_sql::expr::VARIADIC_ARG_FUNCTION.into(),
        binding: None,
        args: vec![value],
        distinct: false,
        order_by: Vec::new(),
        filter: None,
    };
    let array = ScalarExpr::Array(vec![ScalarExpr::Literal(Value::Int(1))]);
    assert_eq!(
        scalar_type(&marker(array.clone()), &RowSchema::default(), &[]).unwrap(),
        Some(ColumnType::Array(Box::new(ColumnType::Integer)))
    );

    let call = ScalarExpr::Func {
        name: "concat".into(),
        binding: None,
        args: vec![
            marker(array),
            ScalarExpr::Literal(Value::Str("tail".into())),
        ],
        distinct: false,
        order_by: Vec::new(),
        filter: None,
    };
    assert!(matches!(
        scalar_type(&call, &RowSchema::default(), &[]),
        Err(SQLError::Internal(message)) if message.contains("final call argument")
    ));

    let malformed = ScalarExpr::Func {
        name: uqa_sql::expr::VARIADIC_ARG_FUNCTION.into(),
        binding: None,
        args: vec![ScalarExpr::Array(vec![ScalarExpr::Literal(Value::Int(1))])],
        distinct: true,
        order_by: Vec::new(),
        filter: None,
    };
    assert!(matches!(
        scalar_type(&malformed, &RowSchema::default(), &[]),
        Err(SQLError::Internal(message)) if message.contains("syntax marker contains function-call metadata")
    ));
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
