//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

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
    let variadic = dispatched(FunctionDispatch::VariadicArgument, vec![array]);
    let named = named_argument("items", variadic);
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
    let marker = |value| dispatched(FunctionDispatch::VariadicArgument, vec![value]);
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

    let mut malformed = dispatched(
        FunctionDispatch::VariadicArgument,
        vec![ScalarExpr::Array(vec![ScalarExpr::Literal(Value::Int(1))])],
    );
    let ScalarExpr::Func { distinct, .. } = &mut malformed else {
        unreachable!();
    };
    *distinct = true;
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
