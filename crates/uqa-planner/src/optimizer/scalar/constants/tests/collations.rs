//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[derive(serde::Deserialize)]
struct Reference {
    field: String,
    null: bool,
    collation: u32,
    value: Option<String>,
}

#[test]
fn selected_constant_fields_retain_postgresql_collations() {
    let cases: Vec<Reference> =
        serde_json::from_str(include_str!("field_collations.json")).unwrap();
    assert_eq!(cases.len(), 4);
    for case in cases {
        let ty = ColumnType::Composite(uqa_sql::ast::CompositeTypeReference {
            schema: "public".into(),
            name: "pair".into(),
            oid: 20_001,
            relation_oid: 20_002,
            array_oid: 20_003,
        });
        let value = if case.null {
            Value::Null
        } else {
            Value::Record(
                vec![
                    ("c".into(), Value::Str("x".into())),
                    ("p".into(), Value::Str("y".into())),
                ]
                .into(),
            )
        };
        let mut expression = bound_call(
            "field selection",
            vec![
                ScalarExpr::TypedLiteral {
                    collation_oid: None,
                    composite_source: None,
                    value,
                    ty: ty.catalog_name(),
                    bound_type: Some(ty),
                    parameter_index: None,
                },
                ScalarExpr::Literal(Value::Str(case.field.clone())),
            ],
        );
        if let ScalarExpr::Func {
            binding: Some(binding),
            ..
        } = &mut expression
        {
            binding.dispatch = Some(uqa_sql::ast::FunctionDispatch::FieldSelect);
            binding.composite_field = Some(Box::new(uqa_sql::ast::CompositeFieldBinding {
                collation_oid: Some(case.collation),
                type_oid: 20_001,
                number: if case.field == "c" { 1 } else { 2 },
                result_type: ColumnType::Text,
                dropped: false,
                changed_type: None,
            }));
        }
        let folded =
            fold_literal_expression(expression, |_| panic!("constant field already read")).unwrap();
        assert_eq!(
            literal_value(&folded),
            Some(&case.value.map_or(Value::Null, Value::Str))
        );
        let stored = serde_json::to_value(&folded).unwrap();
        assert_eq!(
            stored["TypedLiteral"]["collation_oid"],
            serde_json::json!(case.collation)
        );
    }
}
