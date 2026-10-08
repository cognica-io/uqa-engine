//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::ast::{CompositeTypeReference, FunctionBinding};
use crate::expr::composites::{CompositeAttribute, CompositeTypeCatalog, CompositeTypeDescriptor};
use std::collections::BTreeMap;
use std::sync::RwLock;
use uqa_core::{ArrayValue, TemporalValue};

fn ty() -> ColumnType {
    ColumnType::Composite(CompositeTypeReference {
        schema: "public".into(),
        name: "pair".into(),
        oid: 20_001,
        relation_oid: 20_002,
        array_oid: 20_003,
    })
}

fn descriptor(second: i16) -> Arc<CompositeTypeDescriptor> {
    Arc::new(CompositeTypeDescriptor {
        type_oid: 20_001,
        relation_oid: 20_002,
        attributes: vec![
            CompositeAttribute {
                name: "a".into(),
                ty: ColumnType::Integer,
                number: 1,
            },
            CompositeAttribute {
                name: "b".into(),
                ty: ColumnType::Text,
                number: second,
            },
        ],
    })
}

struct Types(RwLock<Arc<CompositeTypeDescriptor>>);
impl FunctionTypeResolver for Types {
    fn resolve_function_type(
        &self,
        _: &str,
        _: Option<&FunctionBinding>,
        _: &[Option<String>],
        _: &[Option<ColumnType>],
        _: bool,
    ) -> Result<Option<ColumnType>, SQLError> {
        Ok(None)
    }
    fn resolve_type_name(&self, name: &str) -> Result<Option<ColumnType>, SQLError> {
        Ok((name == "pair").then(ty))
    }
    fn composite_types(&self) -> Option<&dyn CompositeTypeCatalog> {
        Some(self)
    }
}
impl CompositeTypeCatalog for Types {
    fn composite_type(&self, oid: u32) -> Result<Option<Arc<CompositeTypeDescriptor>>, SQLError> {
        assert_eq!(oid, 20_001);
        Ok(Some(self.0.read().unwrap().clone()))
    }
}

fn value() -> Value {
    Value::Record(vec![
        ("a".into(), Value::Int(1)),
        ("b".into(), Value::Str("x".into())),
    ])
}

#[test]
fn retained_fixed_width_fields_use_postgresql_datum_bits_and_restore_after_rollback() {
    for (before, after, input, expected) in [
        (
            ColumnType::Integer,
            ColumnType::Real,
            Value::Int(1_065_353_216),
            Value::Float(1.0),
        ),
        (
            ColumnType::Real,
            ColumnType::Integer,
            Value::Float(1.0),
            Value::Int(1_065_353_216),
        ),
        (
            ColumnType::Integer,
            ColumnType::Oid,
            Value::Int(-1),
            Value::Int(4_294_967_295),
        ),
        (
            ColumnType::Oid,
            ColumnType::Integer,
            Value::Int(4_294_967_295),
            Value::Int(-1),
        ),
        (
            ColumnType::BigInteger,
            ColumnType::DoublePrecision,
            Value::Int(4_607_182_418_800_017_408),
            Value::Float(1.0),
        ),
        (
            ColumnType::DoublePrecision,
            ColumnType::BigInteger,
            Value::Float(1.0),
            Value::Int(4_607_182_418_800_017_408),
        ),
        (
            ColumnType::Integer,
            ColumnType::Date,
            Value::Int(0),
            Value::Temporal(TemporalValue::Date { days: 10_957 }),
        ),
        (
            ColumnType::Date,
            ColumnType::Integer,
            Value::Temporal(TemporalValue::Date { days: 10_957 }),
            Value::Int(0),
        ),
        (
            ColumnType::BigInteger,
            ColumnType::TimePrecision(0),
            Value::Int(3_723_456_789),
            Value::Temporal(TemporalValue::Time {
                micros: 3_723_456_789,
            }),
        ),
        (
            ColumnType::InternalChar,
            ColumnType::Boolean,
            Value::Str("A".into()),
            Value::Bool(true),
        ),
    ] {
        let mut original = descriptor(2).as_ref().clone();
        original.attributes[0].ty = before;
        let mut current = original.clone();
        current.attributes[0].ty = after;
        let original = BTreeMap::from([(20_001, Arc::new(original))]);
        let current = BTreeMap::from([(20_001, Arc::new(current))]);
        let record = |value| {
            Value::Record(vec![
                ("a".into(), value),
                ("b".into(), Value::Str("tail".into())),
            ])
        };
        let input = record(input);
        assert_eq!(
            project(&input, &ty(), &original, &current).unwrap(),
            record(expected)
        );
        assert_eq!(project(&input, &ty(), &original, &original).unwrap(), input);
        assert_eq!(
            project(&record(Value::Null), &ty(), &original, &current).unwrap(),
            record(Value::Null)
        );
    }
}

#[test]
fn projection_uses_original_numbers_preserves_array_bounds_and_can_restore_after_rollback() {
    let original = BTreeMap::from([(20_001, descriptor(2))]);
    let current = BTreeMap::from([(20_001, descriptor(3))]);
    let input =
        Value::Array(ArrayValue::with_lower_bounds(vec![value(), Value::Null], vec![-2]).unwrap());
    let array_type = ColumnType::Array(Box::new(ty()));
    let projected = project(&input, &array_type, &original, &current).unwrap();
    assert_eq!(
        projected,
        Value::Array(
            ArrayValue::with_lower_bounds(
                vec![
                    Value::Record(vec![("a".into(), Value::Int(1)), ("b".into(), Value::Null)]),
                    Value::Null
                ],
                vec![-2]
            )
            .unwrap()
        )
    );
    assert_eq!(
        project(&input, &array_type, &original, &original).unwrap(),
        input
    );
}

#[test]
fn generic_cache_keeps_source_datums_when_current_type_interpretation_loses_bits() {
    let mut source = descriptor(2).as_ref().clone();
    source.attributes[0].ty = ColumnType::InternalChar;
    let source = Arc::new(source);
    let types = Types(RwLock::new(source.clone()));
    let mut plan = UnifiedPlan::lower(crate::compile("SELECT 1").unwrap().remove(0));
    let raw = Value::Record(vec![
        ("a".into(), Value::Str("A".into())),
        ("b".into(), Value::Str("tail".into())),
    ]);
    plan.rewrite_scalar_expressions(&mut |node| {
        if matches!(node, ScalarExpr::Literal(Value::Int(1))) {
            *node = ScalarExpr::TypedLiteral {
                composite_source: None,
                value: raw.clone(),
                ty: "pair".into(),
                bound_type: Some(ty()),
                parameter_index: None,
            };
        }
    });
    let inputs = CompositeInputs::capture(&plan, &types).unwrap();
    let mut changed = source.as_ref().clone();
    changed.attributes[0].ty = ColumnType::Boolean;
    *types.0.write().unwrap() = Arc::new(changed);
    let generic = inputs.project_for_generic(&plan, &types).unwrap().unwrap();
    let inputs = inputs.with_generic(&generic, &types).unwrap();
    let values = |plan: &UnifiedPlan| {
        let mut values = Vec::new();
        plan.visit_scalar_expressions(&mut |expression| {
            expression.visit(&mut |node| {
                if let ScalarExpr::TypedLiteral { value, .. } = node {
                    values.push(value.clone());
                }
            });
        });
        values
    };
    assert_eq!(values(&generic), std::slice::from_ref(&raw));
    let executed = inputs.project_generic(&generic, &types).unwrap().unwrap();
    assert_eq!(
        values(&executed),
        [Value::Record(vec![
            ("a".into(), Value::Bool(true)),
            ("b".into(), Value::Str("tail".into()))
        ])]
    );
    *types.0.write().unwrap() = source;
    let restored = inputs.project_generic(&generic, &types).unwrap().unwrap();
    assert_eq!(values(&restored), [raw]);
}

#[test]
fn nested_constants_capture_descriptors_and_cached_variants_follow_current_numbers() {
    let types = Types(RwLock::new(descriptor(2)));
    let mut plan = UnifiedPlan::lower(crate::compile("SELECT 1").unwrap().remove(0));
    plan.rewrite_scalar_expressions(&mut |node| {
        if matches!(node, ScalarExpr::Literal(Value::Int(1))) {
            *node = ScalarExpr::Cast {
                implicit: false,
                ty: "pair".into(),
                expr: Box::new(ScalarExpr::TypedLiteral {
                    composite_source: None,
                    value: value(),
                    ty: "pair".into(),
                    bound_type: Some(ty()),
                    parameter_index: None,
                }),
            };
        }
    });
    let inputs = CompositeInputs::capture(&plan, &types).unwrap();
    assert_eq!(inputs.original.len(), 1);
    assert!(inputs.project_logical(&plan, &types).unwrap().is_none());
    let inputs = inputs.with_generic(&plan, &types).unwrap();
    assert!(inputs.project_generic(&plan, &types).unwrap().is_none());
    *types.0.write().unwrap() = descriptor(3);
    assert!(inputs.project_logical(&plan, &types).unwrap().is_some());
    assert!(inputs.project_generic(&plan, &types).unwrap().is_some());
    let folded = UnifiedPlan::lower(crate::compile("SELECT 'x'::text").unwrap().remove(0));
    let scalar_cache = inputs.with_generic(&folded, &types).unwrap();
    assert!(scalar_cache
        .project_generic(&folded, &types)
        .unwrap()
        .is_none());
    *types.0.write().unwrap() = descriptor(2);
    assert!(scalar_cache.generic_requires_rebuild(&types).unwrap());
    assert!(!inputs.generic_requires_rebuild(&types).unwrap());
    assert!(inputs.project_logical(&plan, &types).unwrap().is_none());
    assert!(inputs.project_generic(&plan, &types).unwrap().is_none());
}

#[test]
fn executable_field_projection_keeps_original_type_and_can_undo_descriptor_changes() {
    let types = Types(RwLock::new(descriptor(2)));
    let mut plan = UnifiedPlan::lower(crate::compile("SELECT (ROW(1, 'x')).b").unwrap().remove(0));
    plan.rewrite_scalar_expressions(&mut |node| {
        if let ScalarExpr::Func {
            binding: Some(binding),
            ..
        } = node
        {
            if binding.dispatch == Some(crate::ast::FunctionDispatch::FieldSelect) {
                binding.composite_field = Some(Box::new(crate::ast::CompositeFieldBinding {
                    type_oid: 20_001,
                    number: 2,
                    result_type: ColumnType::Text,
                    dropped: false,
                    changed_type: None,
                }));
            }
        }
    });
    let inputs = CompositeInputs::capture(&plan, &types).unwrap();
    for ty in [ColumnType::Varchar(Some(8)), ColumnType::Varchar(Some(2))] {
        let mut current = descriptor(2).as_ref().clone();
        current.attributes[1].ty = ty.clone();
        *types.0.write().unwrap() = Arc::new(current);
        let projected = inputs.project_logical(&plan, &types).unwrap().unwrap();
        let mut fields = 0;
        projected.visit_scalar_expressions(&mut |expression| {
            expression.visit(&mut |node| {
                if let ScalarExpr::Func {
                    binding: Some(binding),
                    ..
                } = node
                {
                    if let Some(field) = &binding.composite_field {
                        fields += 1;
                        assert_eq!(field.result_type, ColumnType::Text);
                        assert_eq!(field.changed_type.as_ref(), Some(&ty));
                    }
                }
            });
        });
        assert_eq!(fields, 1);
    }
    *types.0.write().unwrap() = descriptor(2);
    assert!(inputs.project_logical(&plan, &types).unwrap().is_none());
}
