//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::ast::CompositeTypeReference;
use crate::expr::composites::{CompositeAttribute, CompositeTypeCatalog, CompositeTypeDescriptor};
use std::{cell::Cell, sync::Arc};
use uqa_core::{memory::MemoryBudget, CancellationToken};

struct Catalog(Cell<bool>);

fn pair() -> ColumnType {
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
        assert_eq!(oid, 20_001);
        let mut attributes = vec![CompositeAttribute {
            name: "a".into(),
            ty: ColumnType::Integer,
            number: 1,
        }];
        if self.0.get() {
            attributes.push(CompositeAttribute {
                name: "b".into(),
                ty: ColumnType::Domain {
                    schema: "public".into(),
                    name: "required".into(),
                    oid: 20_010,
                    array_oid: None,
                    base: Box::new(ColumnType::Integer),
                },
                number: 2,
            });
        }
        Ok(Some(Arc::new(CompositeTypeDescriptor {
            type_oid: oid,
            relation_oid: 20_003,
            attributes,
        })))
    }
}

impl EngineHook for Catalog {
    fn nextval(&self, _: &str) -> Result<i64, SQLError> {
        unreachable!()
    }
    fn currval(&self, _: &str) -> Result<i64, SQLError> {
        unreachable!()
    }
    fn setval(&self, _: &str, _: i64, _: bool) -> Result<i64, SQLError> {
        unreachable!()
    }
    fn resolve_type_name(&self, name: &str) -> Result<Option<ColumnType>, String> {
        Ok(Some(if name.ends_with("[]") {
            ColumnType::Array(Box::new(pair()))
        } else {
            pair()
        }))
    }
    fn composite_types(&self) -> Option<&dyn CompositeTypeCatalog> {
        Some(self)
    }
    fn cast_domain(
        &self,
        _: &Value,
        _: Option<&str>,
        _: &ColumnType,
    ) -> Result<Option<Value>, SQLError> {
        panic!("a retained datum must not repeat domain input or constraints")
    }
}

#[test]
fn retained_constant_observes_addition_and_rollback_without_mutating_its_input() {
    let catalog = Catalog(Cell::new(false));
    let value = Value::Record(vec![("a".into(), Value::Int(4))]);
    let original = value.clone();
    let memory = MemoryBudget::new(4096);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    for added in [false, true, false] {
        catalog.0.set(added);
        let output =
            evaluate_with_control(&value, &pair().catalog_name(), Some(&catalog), &control)
                .unwrap();
        let mut expected = vec![("a".into(), Value::Int(4))];
        if added {
            expected.push(("b".into(), Value::Null));
        }
        assert_eq!(*output, Value::Record(expected));
        assert_eq!(value, original);
        assert!(memory.used() > 0);
        drop(output);
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn retained_array_preserves_bounds_and_releases_memory_on_admission_or_cancellation_errors() {
    let catalog = Catalog(Cell::new(true));
    let value = Value::Array(
        ArrayValue::with_lower_bounds(
            vec![
                Value::Record(vec![("a".into(), Value::Int(4))]),
                Value::Null,
            ],
            vec![-2],
        )
        .unwrap(),
    );
    let ty = ColumnType::Array(Box::new(pair())).catalog_name();
    let token = CancellationToken::new();
    let memory = MemoryBudget::new(4096);
    let control = ProductionControl::new(&memory, &token, &token);
    let output = evaluate_with_control(&value, &ty, Some(&catalog), &control).unwrap();
    let Value::Array(array) = &*output else {
        panic!("array")
    };
    assert_eq!(array.lower_bounds(), [-2]);
    assert_eq!(
        array.elements(),
        [
            Value::Record(vec![("a".into(), Value::Int(4)), ("b".into(), Value::Null)]),
            Value::Null
        ]
    );
    drop(output);
    assert_eq!(memory.used(), 0);
    let small = MemoryBudget::new(8);
    let control = ProductionControl::new(&small, &token, &token);
    assert_eq!(
        evaluate_with_control(&value, &ty, Some(&catalog), &control)
            .unwrap_err()
            .sqlstate(),
        Some("53200")
    );
    assert_eq!(small.used(), 0);
    token.cancel();
    assert_eq!(
        evaluate_with_control(&value, &ty, Some(&catalog), &control)
            .unwrap_err()
            .sqlstate(),
        Some("57014")
    );
    assert_eq!(small.used(), 0);
}
