//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::expr::enums::{EnumLabelCatalog, EnumTypeLabel, EnumTypeLabels};
use std::sync::Arc;
use uqa_core::EnumLabelKey;

#[test]
fn interval_input_applies_fields_before_parameter_type_modifiers_are_removed() {
    let expression = ScalarExpr::Literal(Value::Str("1-2".into()));
    let target = ColumnType::from_sql_name("interval year").unwrap();
    let mut parameters = ParameterTypes::with_input_constants(&[], None, None, None);
    let mut inferred = ExpressionType::unknown_literal(&expression, "1-2".into());
    parameters.coerce_unknown(&mut inferred, &target).unwrap();
    assert_eq!(inferred.ty, Some(ColumnType::Interval));
    assert!(matches!(
        parameters.take_literal(&expression).unwrap(),
        ScalarExpr::TypedLiteral {
            value: Value::Temporal(uqa_core::TemporalValue::Interval {
                months: 12,
                days: 0,
                micros: 0
            }),
            ..
        }
    ));
}

#[test]
fn copied_membership_constants_keep_input_cache_volatility() {
    for (text, target, reusable) in [
        ("1", ColumnType::Integer, true),
        ("2020-02-03", ColumnType::Date, false),
    ] {
        let expression = ScalarExpr::Literal(Value::Str(text.into()));
        let mut parameters = ParameterTypes::with_input_constants(&[], None, None, None);
        parameters
            .coerce_unknown(
                &mut ExpressionType::unknown_literal(&expression, text.into()),
                &target,
            )
            .unwrap();
        let constant = parameters.take_literal(&expression).unwrap();
        parameters.retain_membership(
            &expression,
            super::super::membership::MembershipAnalysis {
                shape: crate::type_resolution::membership::MembershipShape::default(),
                array_coercions: Vec::new(),
                left_constants: vec![Some(constant)],
            },
        );
        let constants = parameters.take_input_constants();
        assert!(constants.0.is_empty());
        assert_eq!(constants.reusable_across_messages(), reusable, "{target:?}");
    }
}

struct Labels(&'static str);

impl EnumLabelCatalog for Labels {
    fn enum_type_labels(&self, oid: u32) -> Result<Option<Arc<EnumTypeLabels>>, SQLError> {
        Ok((oid == 16_384).then(|| {
            Arc::new(EnumTypeLabels {
                type_oid: oid,
                labels: vec![EnumTypeLabel {
                    oid: 16_386,
                    key: EnumLabelKey::from_bytes(vec![0x80]).unwrap(),
                    label: self.0.into(),
                }],
            })
        }))
    }
    fn enum_label_uncommitted(&self, _: u32) -> bool {
        false
    }
    fn enum_type_name(&self, _: u32) -> Result<Option<String>, SQLError> {
        Ok(Some("color".into()))
    }
    fn has_enum_types(&self) -> bool {
        true
    }
}

#[test]
fn retained_enum_inputs_keep_label_identity_and_array_bounds_after_rename() {
    let scalar = ColumnType::Enum(crate::ast::EnumTypeReference {
        schema: "public".into(),
        name: "color".into(),
        oid: 16_384,
        array_oid: 16_385,
    });
    for (sql, target) in [
        ("SELECT 'old'", scalar.clone()),
        (
            "SELECT '[0:1]={old,NULL}'",
            ColumnType::Array(Box::new(scalar)),
        ),
    ] {
        let mut plan = UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0));
        let mut parameters =
            ParameterTypes::with_input_constants(&[], None, Some(&Labels("old")), None);
        plan.visit_scalar_expressions(&mut |expression| {
            let ScalarExpr::Literal(Value::Str(text)) = expression else {
                panic!("literal");
            };
            parameters
                .coerce_unknown(
                    &mut ExpressionType::unknown_literal(expression, text.clone()),
                    &target,
                )
                .unwrap();
        });
        parameters.take_input_constants().apply(&mut plan).unwrap();
        plan.visit_scalar_expressions(&mut |expression| {
            let ScalarExpr::TypedLiteral { value, .. } = expression else {
                panic!("typed input");
            };
            let renamed =
                crate::expr::enums::render_enum_labels(Some(&Labels("new")), value).unwrap();
            match renamed {
                Value::Str(text) => assert_eq!(text, "new"),
                Value::Array(array) => {
                    assert_eq!(array.lower_bounds(), [0]);
                    assert_eq!(array.elements(), [Value::Str("new".into()), Value::Null]);
                }
                other => panic!("unexpected enum output: {other:?}"),
            }
        });
    }
}

fn domain_array() -> ColumnType {
    ColumnType::Array(Box::new(ColumnType::Domain {
        schema: "public".into(),
        name: "positive".into(),
        oid: 16_384,
        array_oid: Some(16_385),
        base: Box::new(ColumnType::Integer),
    }))
}

#[derive(Default)]
struct Inputs(std::cell::Cell<usize>);

impl crate::expr::CatalogInputFunctions for Inputs {
    fn read_unknown_input(&self, text: &str, target: &ColumnType) -> Result<Value, SQLError> {
        assert_eq!(*target, domain_array());
        self.0.set(self.0.get() + 1);
        crate::expr::cast_value_from(&Value::Str(text.into()), "integer[]", None)
    }
}

#[test]
fn domain_array_input_is_read_once_and_retains_element_identity_and_bounds() {
    let inputs = Inputs::default();
    let mut parameters = ParameterTypes::with_input_constants(&[], None, None, Some(&inputs));
    let mut plan = UnifiedPlan::lower(crate::compile("SELECT '[-1:0]={1,2}'").unwrap().remove(0));
    plan.visit_scalar_expressions(&mut |expression| {
        let ScalarExpr::Literal(Value::Str(text)) = expression else {
            panic!("unknown literal")
        };
        // An alias can revisit the same source leaf; its input effects belong to
        // the original leaf, not to the number of metadata consumers.
        for _ in 0..2 {
            let mut inferred = ExpressionType::unknown_literal(expression, text.clone());
            parameters
                .coerce_unknown(&mut inferred, &domain_array())
                .unwrap();
            assert_eq!(inferred.ty, Some(domain_array()));
        }
    });
    assert_eq!(inputs.0.get(), 1);
    parameters.take_input_constants().apply(&mut plan).unwrap();
    plan.visit_scalar_expressions(&mut |expression| {
        let ScalarExpr::TypedLiteral {
            value: Value::Array(array),
            bound_type,
            ..
        } = expression
        else {
            panic!("retained domain array")
        };
        assert_eq!(bound_type.as_ref(), Some(&domain_array()));
        assert_eq!(array.elements(), [Value::Int(1), Value::Int(2)]);
        assert_eq!(array.lower_bounds(), [-1]);
    });
}

#[test]
fn domain_array_metadata_inference_does_not_invoke_or_require_input_functions() {
    let inputs = Inputs::default();
    let mut parameters = ParameterTypes::new(&[]);
    parameters.catalog_inputs = Some(&inputs);
    let expression = ScalarExpr::Literal(Value::Str("{not read}".into()));
    let mut inferred = ExpressionType::unknown_literal(&expression, "{not read}".into());
    parameters
        .coerce_unknown(&mut inferred, &domain_array())
        .unwrap();
    assert_eq!(inferred.ty, Some(domain_array()));
    assert_eq!(inputs.0.get(), 0);
    assert!(parameters.take_input_constants().0.is_empty());
}

#[test]
fn retaining_domain_array_input_requires_the_catalog_capability() {
    let mut parameters = ParameterTypes::with_input_constants(&[], None, None, None);
    let expression = ScalarExpr::Literal(Value::Str("{1}".into()));
    let mut inferred = ExpressionType::unknown_literal(&expression, "{1}".into());
    let failure = parameters
        .coerce_unknown(&mut inferred, &domain_array())
        .unwrap_err();
    assert!(failure
        .to_string()
        .contains("catalog input functions are unavailable"));
    assert!(parameters.take_input_constants().0.is_empty());
}

#[test]
fn scalar_domain_base_input_and_explicit_text_do_not_invoke_array_input() {
    let inputs = Inputs::default();
    let mut parameters = ParameterTypes::with_input_constants(&[], None, None, Some(&inputs));
    let mut expression = ScalarExpr::Literal(Value::Str("0".into()));
    let ColumnType::Array(domain) = domain_array() else {
        unreachable!()
    };
    let mut inferred = ExpressionType::unknown_literal(&expression, "0".into());
    parameters.coerce_unknown(&mut inferred, &domain).unwrap();
    assert_eq!(inferred.ty.as_ref(), Some(domain.as_ref()));
    parameters
        .take_input_constants()
        .apply_expression(&mut expression)
        .unwrap();
    assert!(matches!(
        expression,
        ScalarExpr::TypedLiteral {
            value: Value::Int(0),
            bound_type: Some(ColumnType::Integer),
            ..
        }
    ));
    let mut typed_text = ExpressionType::resolved(Some(ColumnType::Text));
    parameters
        .coerce_unknown(&mut typed_text, &domain_array())
        .unwrap();
    assert_eq!(typed_text.ty, Some(ColumnType::Text));
    assert_eq!(inputs.0.get(), 0);
}
