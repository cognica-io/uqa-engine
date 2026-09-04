//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    eval_call_arguments, eval_scalar, scalar_call_arguments, ScalarEvalContext, ScalarExpr,
};
use crate::{ColumnIdentity, PhysicalRow, RowSchema};
use uqa_core::Value;
use uqa_sql::ast::{BinaryOp, ColumnType, FunctionBinding, FunctionDispatch};
use uqa_sql::{SQLError, SQLParam};

#[test]
fn arithmetic_does_not_require_parser_ast() {
    let expression = ScalarExpr::Binary {
        op: BinaryOp::Multiply,
        lhs: Box::new(ScalarExpr::Literal(Value::Int(7))),
        rhs: Box::new(ScalarExpr::Literal(Value::Int(3))),
    };
    assert_eq!(
        eval_scalar(&expression, &ScalarEvalContext::new(None, &[])).unwrap(),
        Value::Int(21)
    );
}

#[test]
fn cast_uses_the_input_schema_declared_source_type() {
    let schema = RowSchema::with_types(vec!["support".into()], vec![Some(ColumnType::Regproc)]);
    let row = PhysicalRow::from_values(vec![Value::Int(0)]);
    let view = schema.view(&row);
    let expression = ScalarExpr::Cast {
        expr: Box::new(ScalarExpr::Column("support".into())),
        ty: "text".into(),
    };
    assert_eq!(
        eval_scalar(
            &expression,
            &ScalarEvalContext::from_row_lookup(&view, &[]).with_row_schema(&schema),
        )
        .unwrap(),
        Value::Str("-".into())
    );
}

#[test]
fn cast_preserves_unknown_type_for_string_literals() {
    let expression = ScalarExpr::Cast {
        expr: Box::new(ScalarExpr::Literal(Value::Str("[1,5)".into()))),
        ty: "int4range".into(),
    };
    assert_eq!(
        eval_scalar(&expression, &ScalarEvalContext::new(None, &[])).unwrap(),
        Value::Str("[1,5)".into())
    );
}

#[test]
fn scalar_relation_stars_and_aliases_materialize_named_whole_rows() {
    let schema = RowSchema::with_identities(
        vec!["a".into(), "b".into()],
        vec![
            ColumnIdentity::qualified("item", "a"),
            ColumnIdentity::qualified("item", "b"),
        ],
        vec![Some(ColumnType::Integer), Some(ColumnType::Text)],
    );
    let row = PhysicalRow::from_values(vec![Value::Int(7), Value::Str("seven".into())]);
    let view = schema.view(&row);
    let context = ScalarEvalContext::from_row_lookup(&view, &[]).with_row_schema(&schema);
    let expected = Value::Record(vec![
        ("a".into(), Value::Int(7)),
        ("b".into(), Value::Str("seven".into())),
    ]);
    assert_eq!(
        eval_scalar(&ScalarExpr::QualifiedStar("item".into()), &context).unwrap(),
        expected
    );
    assert_eq!(
        eval_scalar(&ScalarExpr::Column("item".into()), &context).unwrap(),
        expected
    );
}

#[test]
fn scalar_whole_rows_read_hidden_physical_identity_aliases() {
    let visible = RowSchema::with_qualified_types(
        "target",
        vec!["i".into(), "xmin".into()],
        vec![Some(ColumnType::Integer), Some(ColumnType::Xid)],
    );
    let visible = RowSchema::with_wildcard_hidden_positions(&visible, [1]);
    let schema = RowSchema::append_hidden_typed(
        &visible,
        &[Some(ColumnType::Integer), Some(ColumnType::Xid)],
    );
    let schema = RowSchema::with_physical_identity_aliases(
        &schema,
        &[
            (
                ColumnIdentity::qualified("new", "i"),
                2,
                Some(ColumnType::Integer),
            ),
            (
                ColumnIdentity::qualified("new", "xmin"),
                3,
                Some(ColumnType::Xid),
            ),
        ],
    );
    let row = PhysicalRow::from_values(vec![
        Value::Int(41),
        Value::Int(1),
        Value::Int(42),
        Value::Int(2),
    ]);
    let view = schema.view(&row);
    let context = ScalarEvalContext::from_row_lookup(&view, &[]).with_row_schema(&schema);

    assert_eq!(
        eval_scalar(&ScalarExpr::Column("new".into()), &context).unwrap(),
        Value::Record(vec![("i".into(), Value::Int(42))])
    );
}

#[test]
fn parameter_zero_is_not_aliased_to_parameter_one() {
    let params = [SQLParam::Scalar(Value::Str("secret".into()))];
    assert!(matches!(
        eval_scalar(
            &ScalarExpr::Param(0),
            &ScalarEvalContext::new(None, &params)
        ),
        Err(SQLError::MissingParam(0))
    ));
}

#[test]
fn typed_scalar_parameter_evaluates_like_scalar() {
    let params = [SQLParam::typed_scalar(
        Value::Int(7),
        ColumnType::SmallInteger,
    )];
    assert_eq!(
        eval_scalar(
            &ScalarExpr::Param(1),
            &ScalarEvalContext::new(None, &params)
        )
        .unwrap(),
        Value::Int(7)
    );
}

#[test]
fn parameter_detection_descends_into_nested_expressions() {
    let expression = ScalarExpr::Func {
        name: "knn_match".into(),
        binding: None,
        args: vec![
            ScalarExpr::Column("embedding".into()),
            ScalarExpr::Array(vec![ScalarExpr::Param(1)]),
            ScalarExpr::Literal(Value::Int(3)),
        ],
        distinct: false,
        order_by: Vec::new(),
        filter: None,
    };

    assert!(expression.contains_parameter());
    assert!(!ScalarExpr::Literal(Value::Int(3)).contains_parameter());
}

#[test]
fn explicit_variadic_call_argument_is_transparent_to_runtime_evaluation() {
    let arguments = vec![marker(
        FunctionDispatch::NamedArgument,
        vec![
            ScalarExpr::Literal(Value::Str("items".into())),
            marker(
                FunctionDispatch::VariadicArgument,
                vec![ScalarExpr::Literal(Value::Int(42))],
            ),
        ],
    )];

    let decoded = scalar_call_arguments(&arguments).unwrap();
    assert_eq!(decoded[0].name, Some("items"));
    assert!(decoded[0].explicit_variadic);
    assert_eq!(decoded[0].value, &ScalarExpr::Literal(Value::Int(42)));
    assert_eq!(
        eval_call_arguments(&arguments, &ScalarEvalContext::new(None, &[])).unwrap(),
        vec![(Some("items".into()), Value::Int(42))]
    );
}

#[test]
fn call_argument_markers_reject_duplicates_and_malformed_nesting() {
    let duplicate = vec![
        marker(
            FunctionDispatch::VariadicArgument,
            vec![ScalarExpr::Literal(Value::Int(1))],
        ),
        marker(
            FunctionDispatch::VariadicArgument,
            vec![ScalarExpr::Literal(Value::Int(2))],
        ),
    ];
    assert!(matches!(
        scalar_call_arguments(&duplicate),
        Err(SQLError::Internal(message)) if message.contains("more than one")
    ));

    let nested = vec![marker(
        FunctionDispatch::VariadicArgument,
        vec![marker(
            FunctionDispatch::VariadicArgument,
            vec![ScalarExpr::Literal(Value::Int(1))],
        )],
    )];
    assert!(matches!(
        scalar_call_arguments(&nested),
        Err(SQLError::Internal(message)) if message.contains("nested")
    ));
}

fn marker(dispatch: FunctionDispatch, args: Vec<ScalarExpr>) -> ScalarExpr {
    let binding = FunctionBinding::dispatched(dispatch);
    ScalarExpr::Func {
        name: binding.name.clone(),
        binding: Some(binding),
        args,
        distinct: false,
        order_by: Vec::new(),
        filter: None,
    }
}
