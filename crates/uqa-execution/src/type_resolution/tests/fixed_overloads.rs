//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::super::*;

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
fn fixed_builtin_binding_uses_typed_sql_parameters_across_families() {
    let param = SQLParam::scalar;
    let int8 = i64::from(i32::MAX) + 1;
    let bytes = || vec![param(Value::Bytes(vec![0, 255]))];
    let ints = |left, right| vec![param(Value::Int(left)), param(Value::Int(right))];
    for (function, parameters, expected_dispatch, expected_arguments, expected_return) in [
        ("md5", bytes(), "md5", vec!["bytea"], ColumnType::Text),
        (
            "reverse",
            bytes(),
            "reverse",
            vec!["bytea"],
            ColumnType::Bytea,
        ),
        (
            "length",
            bytes(),
            "length",
            vec!["bytea"],
            ColumnType::Integer,
        ),
        (
            "to_hex",
            vec![param(Value::Int(42))],
            TO_HEX_INT4_FUNCTION,
            vec!["integer"],
            ColumnType::Text,
        ),
        (
            "to_hex",
            vec![param(Value::Int(int8))],
            TO_HEX_INT8_FUNCTION,
            vec!["bigint"],
            ColumnType::Text,
        ),
        (
            "random",
            ints(1, 2),
            RANDOM_INT4_FUNCTION,
            vec!["integer", "integer"],
            ColumnType::Integer,
        ),
        (
            "random",
            ints(1, int8),
            RANDOM_INT8_FUNCTION,
            vec!["bigint", "bigint"],
            ColumnType::BigInteger,
        ),
        (
            "random",
            vec![
                param(Value::Int(int8)),
                param(Value::Decimal(uqa_core::DecimalValue::from_i64(3))),
            ],
            RANDOM_NUMERIC_FUNCTION,
            vec!["numeric", "numeric"],
            ColumnType::Numeric {
                precision: None,
                scale: None,
            },
        ),
        (
            "json_strip_nulls",
            vec![param(Value::Str("{}".into())), param(Value::Bool(true))],
            "json_strip_nulls",
            vec!["json", "boolean"],
            ColumnType::Json,
        ),
        (
            "uuid_extract_version",
            vec![param(Value::Str(
                "00000000-0000-4000-8000-000000000000".into(),
            ))],
            "uuid_extract_version",
            vec!["uuid"],
            ColumnType::SmallInteger,
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
        assert_eq!(name, expected_dispatch, "{function}");
        let expected_arguments = expected_arguments
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<_>>();
        let binding = binding.unwrap_or_else(|| panic!("{function} must retain its binding"));
        assert_eq!(binding.name, format!("pg_catalog.{function}"), "{function}");
        assert!(binding.builtin, "{function}");
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
