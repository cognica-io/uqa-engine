//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::ast::{CompositeTypeReference, FunctionBinding};
use crate::expr::composites::{CompositeAttribute, CompositeTypeDescriptor};
use std::sync::Arc;
use uqa_core::ArrayValue;

struct Catalog;

fn pair_type() -> ColumnType {
    ColumnType::Composite(CompositeTypeReference {
        schema: "public".into(),
        name: "pair".into(),
        oid: 20_001,
        array_oid: 20_002,
        relation_oid: 20_003,
    })
}

impl CompositeTypeCatalog for Catalog {
    fn composite_type(&self, oid: u32) -> Result<Option<Arc<CompositeTypeDescriptor>>, SQLError> {
        Ok((oid == 20_001).then(|| {
            Arc::new(CompositeTypeDescriptor {
                type_oid: oid,
                relation_oid: 20_003,
                attributes: vec![CompositeAttribute {
                    name: "a".into(),
                    ty: ColumnType::Integer,
                    number: 1,
                }],
            })
        }))
    }
}

impl FunctionTypeResolver for Catalog {
    fn resolve_type_name(&self, name: &str) -> Result<Option<ColumnType>, SQLError> {
        let pair = pair_type();
        let array = ColumnType::Array(Box::new(pair.clone()));
        Ok([pair, array]
            .into_iter()
            .find(|ty| ty.catalog_name() == name))
    }

    fn resolve_function_type(
        &self,
        _: &str,
        _: Option<&FunctionBinding>,
        _: &[Option<String>],
        _: &[Option<ColumnType>],
        _: bool,
    ) -> Result<Option<ColumnType>, SQLError> {
        panic!("rewriting a stored datum must not bind or evaluate functions")
    }
}

fn change(change: &AttributeChange) -> CompositeConstantChange<'_> {
    CompositeConstantChange {
        target: 20_001,
        change,
        catalog: &Catalog,
        types: &Catalog,
        rename: None,
    }
}

fn value() -> Value {
    Value::Record(vec![("a".into(), Value::Int(4))])
}

#[test]
fn stored_input_datums_reinterpret_bits_without_rebinding_or_reapplying_input() {
    let array = ColumnType::Array(Box::new(pair_type()));
    let mut value = Value::Array(
        ArrayValue::with_lower_bounds(
            vec![
                Value::Record(vec![("a".into(), Value::Int(1_065_353_216))]),
                Value::Null,
            ],
            vec![-2],
        )
        .unwrap(),
    );
    let type_change = AttributeChange::Type {
        name: "a".into(),
        from: Box::new(ColumnType::Integer),
        to: Box::new(ColumnType::Real),
    };
    assert!(change(&type_change).value(&mut value, &array).unwrap());
    let Value::Array(value) = value else {
        panic!("array carrier")
    };
    assert_eq!(value.lower_bounds(), &[-2]);
    assert_eq!(
        value.elements(),
        &[
            Value::Record(vec![("a".into(), Value::Float(1.0))]),
            Value::Null,
        ]
    );
}

#[test]
fn typed_datums_expand_inside_subqueries_without_rebinding_ordinary_input_casts() {
    let mut statement = crate::compile("SELECT (SELECT 4), '(4)'::text")
        .unwrap()
        .remove(0);
    crate::catalog::stored_ast::visit_stored_statement_expressions(&mut statement, &mut |node| {
        if *node == Expr::Literal(Value::Int(4)) {
            *node = Expr::TypedLiteral {
                value: value(),
                ty: pair_type().catalog_name(),
            };
        }
        Ok(())
    })
    .unwrap();
    let added = AttributeChange::Add("b".into());
    let change = change(&added);
    let mut plan = UnifiedPlan::lower(statement.clone());
    assert!(change.statement(&mut statement).unwrap());
    assert!(change.plan(&mut plan).unwrap());
    assert_eq!(
        serde_json::to_value(&plan).unwrap(),
        serde_json::to_value(UnifiedPlan::lower(statement)).unwrap()
    );
    let mut seen = 0;
    plan.rewrite_scalar_expressions(&mut |node| {
        if let ScalarExpr::TypedLiteral { value, ty, .. } = node {
            seen += 1;
            assert_eq!(*ty, pair_type().catalog_name());
            assert_eq!(
                *value,
                Value::Record(vec![("a".into(), Value::Int(4)), ("b".into(), Value::Null)])
            );
        }
    });
    assert_eq!(seen, 1);
}

#[test]
fn array_constants_keep_bounds_nulls_and_existing_fields() {
    let added = AttributeChange::Add("b".into());
    let change = change(&added);
    let mut expression = Expr::TypedLiteral {
        value: Value::Array(
            ArrayValue::with_lower_bounds(vec![value(), Value::Null], vec![-3]).unwrap(),
        ),
        ty: ColumnType::Array(Box::new(pair_type())).catalog_name(),
    };
    assert!(change.expression(&mut expression).unwrap());
    let Expr::TypedLiteral {
        value: Value::Array(array),
        ..
    } = expression
    else {
        panic!("array datum")
    };
    assert_eq!(array.lower_bounds(), [-3]);
    assert_eq!(array.dimensions(), [2]);
    assert_eq!(
        array.elements(),
        [
            Value::Record(vec![("a".into(), Value::Int(4)), ("b".into(), Value::Null)]),
            Value::Null
        ]
    );
}

#[test]
fn missing_durable_type_identity_is_rejected_without_discarding_the_value() {
    let mut expression = Expr::TypedLiteral {
        value: value(),
        ty: "composite#99999".into(),
    };
    let before = expression.clone();
    assert!(change(&AttributeChange::Add("b".into()))
        .expression(&mut expression)
        .unwrap_err()
        .to_string()
        .contains("disappeared"));
    assert_eq!(expression, before);
}
