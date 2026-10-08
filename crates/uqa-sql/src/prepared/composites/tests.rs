//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::ast::{CompositeTypeReference, FunctionBinding};
use crate::expr::composites::{CompositeAttribute, CompositeTypeCatalog};
use std::sync::RwLock;

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
fn nested_constants_capture_descriptors_and_cached_variants_follow_current_numbers() {
    let types = Types(RwLock::new(descriptor(2)));
    let mut plan = UnifiedPlan::lower(crate::compile("SELECT 1").unwrap().remove(0));
    plan.rewrite_scalar_expressions(&mut |node| {
        if matches!(node, ScalarExpr::Literal(Value::Int(1))) {
            *node = ScalarExpr::Cast {
                implicit: false,
                ty: "pair".into(),
                expr: Box::new(ScalarExpr::TypedLiteral {
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
