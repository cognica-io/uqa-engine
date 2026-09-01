//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::super::*;
use uqa_sql::ast::FunctionDispatch;

#[test]
fn compatibility_resolvers_accept_mixed_case_pg_catalog_qualification() {
    let positional: [Option<String>; 1] = [None];
    let bytea = [Some(ColumnType::Bytea)];
    for (name, resolved) in [
        (
            "md5",
            resolve_md5_overload("PG_CATALOG.MD5", None, &positional, &bytea, None),
        ),
        (
            "reverse",
            resolve_reverse_overload("PG_CATALOG.REVERSE", None, &positional, &bytea, None),
        ),
        (
            "length",
            resolve_length_overload("PG_CATALOG.LENGTH", None, &positional, &bytea, None),
        ),
        (
            "crc32c",
            resolve_checksum_overload("PG_CATALOG.CRC32C", None, &positional, &bytea, None),
        ),
    ] {
        assert_eq!(
            resolved.unwrap(),
            ResolvedStringBinaryOverload::Builtin(ColumnType::Bytea),
            "{name}"
        );
    }

    for (name, resolved, expected_binding, expected_return) in [
        (
            "gamma",
            resolve_gamma_overload(
                "PG_CATALOG.GAMMA",
                None,
                &positional,
                &[Some(ColumnType::Integer)],
                None,
            ),
            "pg_catalog.gamma",
            ColumnType::DoublePrecision,
        ),
        (
            "jsonb_strip_nulls",
            resolve_json_strip_overload(
                "PG_CATALOG.JSONB_STRIP_NULLS",
                None,
                &positional,
                &[Some(ColumnType::JsonB)],
                None,
            ),
            "pg_catalog.jsonb_strip_nulls",
            ColumnType::JsonB,
        ),
    ] {
        let resolved = resolved.unwrap();
        assert_eq!(resolved.binding.name, expected_binding, "{name}");
        assert_eq!(resolved.return_type, expected_return, "{name}");
        assert!(resolved.binding.builtin, "{name}");
    }
}

#[test]
fn non_fixed_udf_introspection_retains_the_resolver_binding() {
    struct StableUdfResolver {
        binding: FunctionBinding,
    }

    impl FunctionTypeResolver for StableUdfResolver {
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

        fn resolve_function_overload(
            &self,
            name: &str,
            binding: Option<&FunctionBinding>,
            argument_names: &[Option<String>],
            argument_types: &[Option<ColumnType>],
            explicit_variadic: bool,
        ) -> Result<Option<ResolvedFunctionOverload>, SQLError> {
            assert_eq!(name, "stable_udf");
            assert_eq!(binding, None);
            assert_eq!(argument_names, [None]);
            assert_eq!(argument_types, [Some(ColumnType::Integer)]);
            assert!(!explicit_variadic);
            Ok(Some(ResolvedFunctionOverload {
                binding: self.binding.clone(),
                return_type: ColumnType::Text,
                exact_matches: 1,
                known_arguments: 1,
                preferred_matches: 0,
                precedes_pg_catalog: true,
            }))
        }

        fn is_scalar_function_binding(&self, binding: &FunctionBinding) -> Result<bool, SQLError> {
            assert_eq!(binding, &self.binding);
            Ok(true)
        }
    }

    let resolver = StableUdfResolver {
        binding: FunctionBinding {
            name: "application.stable_udf".into(),
            argument_types: vec!["integer".into()],
            builtin: false,
            dispatch: None,
            invocation: None,
            resolution_error: None,
        },
    };
    let parameters = [SQLParam::Scalar(Value::Int(7))];
    let expression = ScalarExpr::Func {
        name: "stable_udf".into(),
        binding: None,
        args: vec![ScalarExpr::Param(1)],
        distinct: false,
        order_by: Vec::new(),
        filter: None,
    };
    let bound = bind_type_introspection_with_resolver(
        expression,
        &RowSchema::default(),
        &parameters,
        &resolver,
    );
    let ScalarExpr::Func {
        binding: Some(binding),
        ..
    } = &bound
    else {
        panic!("ordinary UDF calls must retain the catalog-selected binding");
    };
    assert_eq!(binding, &resolver.binding);
}

#[test]
fn non_fixed_udf_introspection_keeps_typed_text_distinct_from_unknown() {
    struct TypedTextResolver;

    impl FunctionTypeResolver for TypedTextResolver {
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

        fn resolve_function_overload(
            &self,
            _name: &str,
            _binding: Option<&FunctionBinding>,
            _argument_names: &[Option<String>],
            argument_types: &[Option<ColumnType>],
            _explicit_variadic: bool,
        ) -> Result<Option<ResolvedFunctionOverload>, SQLError> {
            assert_eq!(argument_types, [Some(ColumnType::Text)]);
            Ok(Some(ResolvedFunctionOverload {
                binding: FunctionBinding {
                    name: "application.typed_text".into(),
                    argument_types: vec!["text".into()],
                    builtin: false,
                    dispatch: None,
                    invocation: None,
                    resolution_error: None,
                },
                return_type: ColumnType::Text,
                exact_matches: 1,
                known_arguments: 1,
                preferred_matches: 0,
                precedes_pg_catalog: true,
            }))
        }

        fn is_scalar_function_binding(&self, _binding: &FunctionBinding) -> Result<bool, SQLError> {
            Ok(true)
        }
    }

    let expression = ScalarExpr::Func {
        name: "typed_text".into(),
        binding: None,
        args: vec![ScalarExpr::Param(1)],
        distinct: false,
        order_by: Vec::new(),
        filter: None,
    };
    let parameters = [SQLParam::typed_scalar(
        Value::Str("value".into()),
        ColumnType::Text,
    )];
    assert!(matches!(
        bind_type_introspection_with_resolver(
            expression,
            &RowSchema::default(),
            &parameters,
            &TypedTextResolver,
        ),
        ScalarExpr::Func {
            binding: Some(_),
            ..
        }
    ));
}

#[test]
fn scalar_introspection_rejects_non_scalar_catalog_bindings() {
    struct NonScalarResolver {
        binding: FunctionBinding,
    }

    impl FunctionTypeResolver for NonScalarResolver {
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

        fn resolve_function_overload(
            &self,
            _name: &str,
            _binding: Option<&FunctionBinding>,
            _argument_names: &[Option<String>],
            _argument_types: &[Option<ColumnType>],
            _explicit_variadic: bool,
        ) -> Result<Option<ResolvedFunctionOverload>, SQLError> {
            Ok(Some(ResolvedFunctionOverload {
                binding: self.binding.clone(),
                return_type: ColumnType::Text,
                exact_matches: 1,
                known_arguments: 1,
                preferred_matches: 0,
                precedes_pg_catalog: true,
            }))
        }

        fn is_scalar_function_binding(&self, binding: &FunctionBinding) -> Result<bool, SQLError> {
            assert_eq!(binding, &self.binding);
            Ok(false)
        }
    }

    for routine_name in ["catalog_aggregate", "catalog_setof"] {
        let resolver = NonScalarResolver {
            binding: FunctionBinding {
                name: format!("application.{routine_name}"),
                argument_types: vec!["integer".into()],
                builtin: false,
                dispatch: None,
                invocation: None,
                resolution_error: None,
            },
        };
        let expression = ScalarExpr::Func {
            name: routine_name.into(),
            binding: None,
            args: vec![ScalarExpr::Literal(Value::Int(7))],
            distinct: false,
            order_by: Vec::new(),
            filter: None,
        };
        let bound = bind_type_introspection_with_resolver(
            expression,
            &RowSchema::default(),
            &[],
            &resolver,
        );
        assert!(
            matches!(bound, ScalarExpr::Func { binding: None, .. }),
            "{routine_name} must not be attached to a scalar call"
        );
    }
}

#[test]
fn scalar_introspection_does_not_resolve_builtin_aggregates_as_catalog_functions() {
    struct UnexpectedResolver;

    impl FunctionTypeResolver for UnexpectedResolver {
        fn resolve_function_type(
            &self,
            _name: &str,
            _binding: Option<&FunctionBinding>,
            _argument_names: &[Option<String>],
            _argument_types: &[Option<ColumnType>],
            _explicit_variadic: bool,
        ) -> Result<Option<ColumnType>, SQLError> {
            panic!("aggregate introspection must not query scalar function types")
        }

        fn resolve_function_overload(
            &self,
            _name: &str,
            _binding: Option<&FunctionBinding>,
            _argument_names: &[Option<String>],
            _argument_types: &[Option<ColumnType>],
            _explicit_variadic: bool,
        ) -> Result<Option<ResolvedFunctionOverload>, SQLError> {
            panic!("aggregate introspection must not bind a scalar catalog function")
        }
    }

    for name in ["sum", "PG_CATALOG.SUM"] {
        let expression = ScalarExpr::Func {
            name: name.into(),
            binding: None,
            args: vec![ScalarExpr::Literal(Value::Null)],
            distinct: false,
            order_by: Vec::new(),
            filter: None,
        };
        let bound = bind_type_introspection_with_resolver(
            expression,
            &RowSchema::default(),
            &[],
            &UnexpectedResolver,
        );
        assert!(matches!(bound, ScalarExpr::Func { binding: None, .. }));
    }
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "covers the fixed overload family matrix"
)]
fn fixed_builtin_binding_uses_typed_sql_parameters_across_families() {
    let param = SQLParam::scalar;
    let int8 = i64::from(i32::MAX) + 1;
    let bytes = || vec![param(Value::Bytes(vec![0, 255]))];
    let ints = |left, right| vec![param(Value::Int(left)), param(Value::Int(right))];
    for (function, parameters, expected_dispatch, expected_arguments, expected_return) in [
        ("md5", bytes(), None, vec!["bytea"], ColumnType::Text),
        ("reverse", bytes(), None, vec!["bytea"], ColumnType::Bytea),
        ("length", bytes(), None, vec!["bytea"], ColumnType::Integer),
        (
            "to_hex",
            vec![param(Value::Int(42))],
            Some(FunctionDispatch::ToHexInt4),
            vec!["integer"],
            ColumnType::Text,
        ),
        (
            "to_hex",
            vec![param(Value::Int(int8))],
            Some(FunctionDispatch::ToHexInt8),
            vec!["bigint"],
            ColumnType::Text,
        ),
        (
            "random",
            ints(1, 2),
            Some(FunctionDispatch::RandomInt4Range),
            vec!["integer", "integer"],
            ColumnType::Integer,
        ),
        (
            "random",
            ints(1, int8),
            Some(FunctionDispatch::RandomInt8Range),
            vec!["bigint", "bigint"],
            ColumnType::BigInteger,
        ),
        (
            "random",
            vec![
                param(Value::Int(int8)),
                param(Value::Decimal(uqa_core::DecimalValue::from_i64(3))),
            ],
            Some(FunctionDispatch::RandomNumericRange),
            vec!["numeric", "numeric"],
            ColumnType::Numeric {
                precision: None,
                scale: None,
            },
        ),
        (
            "json_strip_nulls",
            vec![param(Value::Str("{}".into())), param(Value::Bool(true))],
            None,
            vec!["json", "boolean"],
            ColumnType::Json,
        ),
        (
            "uuid_extract_version",
            vec![param(Value::Str(
                "00000000-0000-4000-8000-000000000000".into(),
            ))],
            None,
            vec!["uuid"],
            ColumnType::SmallInteger,
        ),
        (
            "to_regproc",
            vec![param(Value::Str("casefold".into()))],
            None,
            vec!["text"],
            ColumnType::Regproc,
        ),
        (
            "to_regprocedure",
            vec![param(Value::Str("casefold(text)".into()))],
            None,
            vec!["text"],
            ColumnType::Regprocedure,
        ),
        (
            "to_regclass",
            vec![param(Value::Str("pg_type".into()))],
            None,
            vec!["text"],
            ColumnType::Regclass,
        ),
        (
            "to_regnamespace",
            vec![param(Value::Str("pg_catalog".into()))],
            None,
            vec!["text"],
            ColumnType::Regnamespace,
        ),
        (
            "to_regrole",
            vec![param(Value::Str("uqa".into()))],
            None,
            vec!["text"],
            ColumnType::Regrole,
        ),
        (
            "to_regtype",
            vec![param(Value::Str("integer".into()))],
            None,
            vec!["text"],
            ColumnType::Regtype,
        ),
    ] {
        let expression = ScalarExpr::Func {
            name: function.into(),
            binding: None,
            args: (1..=parameters.len()).map(ScalarExpr::Param).collect(),
            distinct: false,
            order_by: Vec::new(),
            filter: None,
        };
        assert_eq!(
            scalar_type(&expression, &RowSchema::default(), &parameters).unwrap(),
            Some(expected_return),
            "{function}"
        );
        let bound = bind_type_introspection(expression, &RowSchema::default(), &parameters);
        let (name, binding, args) = match bound {
            ScalarExpr::Func {
                name,
                binding,
                args,
                ..
            } => (name, binding, args),
            other => panic!("{function} must retain a bound scalar call: {other:?}"),
        };
        assert_eq!(name, function, "{function}");
        let expected_arguments = expected_arguments
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<_>>();
        let binding = binding.unwrap_or_else(|| panic!("{function} must retain its binding"));
        assert_eq!(binding.name, format!("pg_catalog.{function}"), "{function}");
        assert!(binding.builtin, "{function}");
        assert_eq!(binding.dispatch, expected_dispatch, "{function}");
        assert_eq!(binding.argument_types, expected_arguments, "{function}");
        for (position, (argument, expected_type)) in
            args.iter().zip(&expected_arguments).enumerate()
        {
            assert!(
                matches!(
                    argument,
                    ScalarExpr::Cast { expr, ty }
                        if ty == expected_type
                            && matches!(expr.as_ref(), ScalarExpr::Param(index) if *index == position + 1)
                ),
                "{function} argument {position} must retain its selected SQL parameter type"
            );
        }
    }
}
