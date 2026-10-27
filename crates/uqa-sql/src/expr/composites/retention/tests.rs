//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    ast::CompositeTypeReference,
    expr::{
        composites::{CompositeAttribute, CompositeTypeDescriptor, DroppedCompositeAttribute},
        EngineHook,
    },
    ColumnType,
};
use std::{cell::RefCell, sync::Arc};
use uqa_core::{
    memory::MemoryBudget, ArrayValue, CancellationToken, RecordFieldType, RecordValue, RowValue,
};

struct Catalog(RefCell<Arc<CompositeTypeDescriptor>>);

impl Catalog {
    fn new() -> Self {
        Self(RefCell::new(Arc::new(CompositeTypeDescriptor {
            type_oid: 20_001,
            relation_oid: 20_003,
            dropped: vec![],
            attributes: ["a", "b", "c"]
                .into_iter()
                .enumerate()
                .map(|(index, name)| CompositeAttribute {
                    name: name.into(),
                    ty: ColumnType::Integer,
                    collation_oid: None,
                    number: index as i16 + 1,
                })
                .collect(),
        })))
    }
}

impl CompositeTypeCatalog for Catalog {
    fn composite_type(&self, oid: u32) -> Result<Option<Arc<CompositeTypeDescriptor>>, SQLError> {
        Ok((oid == 20_001).then(|| Arc::clone(&self.0.borrow())))
    }
}

impl EngineHook for Catalog {
    fn nextval(&self, _: &str) -> Result<i64, SQLError> {
        panic!("retention cannot repeat input effects")
    }
    fn currval(&self, _: &str) -> Result<i64, SQLError> {
        unreachable!()
    }
    fn setval(&self, _: &str, _: i64, _: bool) -> Result<i64, SQLError> {
        unreachable!()
    }
    fn composite_types(&self) -> Option<&dyn CompositeTypeCatalog> {
        Some(self)
    }
    fn resolve_type_oid(&self, oid: u32) -> Result<Option<ColumnType>, String> {
        Ok((oid == 20_001).then(|| {
            ColumnType::Composite(CompositeTypeReference {
                schema: "public".into(),
                name: "triple".into(),
                oid,
                array_oid: 20_002,
                relation_oid: 20_003,
            })
        }))
    }
}

fn source() -> Value {
    Value::Record(RecordValue::from_parts(
        [("a", 16_909_060), ("b", 84_281_096), ("c", 287_454_020)]
            .into_iter()
            .map(|(name, value)| (name.into(), Value::Int(value)))
            .collect(),
        Some(20_001),
    ))
}

#[test]
fn selected_physical_fields_do_not_read_unreached_headers_or_null_payloads() {
    let catalog = Catalog::new();
    let control = ProductionControl::uncontrolled();
    let value = retain_value_with_control(
        control.copy_value(&source()).unwrap(),
        Some(&catalog),
        &control,
    )
    .unwrap();
    let Value::Datum(datum) = &*value else {
        panic!("physical input")
    };
    Arc::make_mut(&mut catalog.0.borrow_mut()).attributes[2].ty = ColumnType::Text;
    let first = crate::expr::datums::read_composite_field_with_control(
        datum,
        "a",
        Some(&catalog),
        &control,
    )
    .unwrap();
    assert_eq!(
        &*decode(&first, &catalog, &control),
        &Value::Int(16_909_060)
    );
    let tail = crate::expr::datums::read_composite_field_with_control(
        datum,
        "c",
        Some(&catalog),
        &control,
    )
    .unwrap();
    let Value::Datum(tail) = &*tail else {
        panic!("deferred tail")
    };
    assert_eq!(
        crate::expr::datums::read_with_control(tail, &control)
            .unwrap_err()
            .sqlstate(),
        Some("XX001")
    );

    let catalog = Catalog::new();
    let source = Value::Record(RecordValue::from_parts(
        vec![
            ("a".into(), Value::Int(7)),
            ("b".into(), Value::Null),
            ("c".into(), Value::Int(9)),
        ],
        Some(20_001),
    ));
    let value = retain_value_with_control(
        control.copy_value(&source).unwrap(),
        Some(&catalog),
        &control,
    )
    .unwrap();
    let Value::Datum(datum) = &*value else {
        panic!("physical nullable input")
    };
    Arc::make_mut(&mut catalog.0.borrow_mut()).attributes[0].ty = ColumnType::Text;
    let empty = MemoryBudget::new(0);
    let token = CancellationToken::new();
    let bounded = ProductionControl::new(&empty, &token, &token);
    assert_eq!(
        &*crate::expr::datums::read_composite_field_with_control(
            datum,
            "b",
            Some(&catalog),
            &bounded
        )
        .unwrap(),
        &Value::Null
    );
    assert_eq!(empty.used(), 0);
    token.cancel();
    assert!(crate::expr::datums::read_composite_field_with_control(
        datum,
        "b",
        Some(&catalog),
        &bounded
    )
    .is_err());
}

#[test]
fn json_keeps_real_field_width_in_logical_and_physical_records() {
    let catalog = Catalog::new();
    {
        let mut descriptor = catalog.0.borrow_mut();
        let descriptor = Arc::make_mut(&mut descriptor);
        descriptor.attributes[0].ty = ColumnType::Real;
        descriptor.attributes[1].ty = ColumnType::DoublePrecision;
    }
    let source = Value::Record(RecordValue::from_parts(
        vec![
            ("a".into(), Value::Float(f64::from(0.1_f32))),
            ("b".into(), Value::Float(0.1)),
            ("c".into(), Value::Int(7)),
        ],
        Some(20_001),
    ));
    let control = ProductionControl::uncontrolled();
    let retained = retain_value_with_control(
        control.copy_value(&source).unwrap(),
        Some(&catalog),
        &control,
    )
    .unwrap();
    let adapter = crate::expr::EngineValueCatalog(&catalog);
    for value in [&source, &*retained] {
        assert_eq!(
            crate::expr::json::value_to_json_text_with_catalog(value, Some(&adapter)).unwrap(),
            r#"{"a":0.1,"b":0.1,"c":7}"#
        );
    }
    let row = Value::Row(
        RowValue::typed(
            vec![Value::Float(f64::from(0.1_f32))],
            vec![RecordFieldType {
                oid: 700,
                type_modifier: -1,
                collation_oid: None,
            }],
        )
        .unwrap(),
    );
    assert_eq!(
        crate::expr::json::value_to_json_text(&row).unwrap(),
        r#"{"f1":0.1}"#
    );
}

#[test]
fn json_observes_retained_tuple_fields_through_the_current_catalog() {
    let catalog = Catalog::new();
    let control = ProductionControl::uncontrolled();
    let value = retain_value_with_control(
        control.copy_value(&source()).unwrap(),
        Some(&catalog),
        &control,
    )
    .unwrap();
    let adapter = crate::expr::EngineValueCatalog(&catalog);
    assert_eq!(
        crate::expr::json::value_to_json_text_with_catalog(&value, Some(&adapter)).unwrap(),
        r#"{"a":16909060,"b":84281096,"c":287454020}"#
    );
    {
        let mut descriptor = catalog.0.borrow_mut();
        let descriptor = Arc::make_mut(&mut descriptor);
        descriptor.attributes[0].name = "renamed".into();
        descriptor
            .dropped
            .push(DroppedCompositeAttribute::from_type(
                3,
                &ColumnType::Integer,
            ));
        descriptor.attributes.pop();
    }
    assert_eq!(
        crate::expr::json::value_to_json_text_with_catalog(&value, Some(&adapter)).unwrap(),
        r#"{"renamed":16909060,"b":84281096}"#
    );
}

#[test]
fn a_host_constructor_keeps_its_descriptor_when_a_field_changes_the_catalog() {
    let catalog = Catalog::new();
    let original = Arc::clone(&catalog.0.borrow());
    let budget = MemoryBudget::new(16384);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let value = super::super::constructor::construct_current_with_control(
        &original,
        Some(&catalog),
        &control,
        |index| {
            if index == 0 {
                Arc::make_mut(&mut catalog.0.borrow_mut()).attributes[0].name = "renamed".into();
            }
            Ok(control.finish(Value::Int(index as i64 + 7), control.empty_reservation())?)
        },
    )
    .unwrap();
    let decoded = decode(&value, &catalog, &control);
    let Value::Record(record) = &*decoded else {
        panic!("decoded record")
    };
    assert_eq!(record.get(0).unwrap().0, "renamed");
    for index in 0..3 {
        assert_eq!(
            *decode(record.get(index).unwrap().1, &catalog, &control),
            Value::Int(index as i64 + 7)
        );
    }
    drop(decoded);
    drop(value);
    assert_eq!(budget.used(), 0);
}

#[test]
fn physical_row_null_tests_read_only_live_attributes_and_the_original_bitmap() {
    let catalog = Catalog::new();
    let budget = MemoryBudget::new(16384);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let value = retain_value_with_control(
        control.copy_value(&source()).unwrap(),
        Some(&catalog),
        &control,
    )
    .unwrap();
    let retained = budget.used();
    for attribute in &mut Arc::make_mut(&mut catalog.0.borrow_mut()).attributes {
        // These bytes are not valid text fields; NULL tests must never read their payloads.
        attribute.ty = ColumnType::Text;
    }
    let test = |negated| {
        crate::expr::sql_null_test_with_control(&value, negated, Some(&catalog), &control).unwrap()
    };
    assert!(!test(false));
    assert!(test(true));
    Arc::make_mut(&mut catalog.0.borrow_mut())
        .attributes
        .push(CompositeAttribute {
            name: "added".into(),
            ty: ColumnType::Text,
            collation_oid: None,
            number: 4,
        });
    assert!(!test(false));
    assert!(!test(true));
    Arc::make_mut(&mut catalog.0.borrow_mut())
        .attributes
        .clear();
    assert!(test(false));
    assert!(test(true));
    assert_eq!(budget.used(), retained);
    drop(value);
    assert_eq!(budget.used(), 0);
}

fn decode(value: &Value, catalog: &Catalog, control: &ProductionControl<'_>) -> Produced<Value> {
    let Value::Datum(datum) = value else {
        panic!("retained Datum")
    };
    crate::expr::datums::read_with_catalog_and_control(datum, Some(catalog), control).unwrap()
}

#[test]
fn physical_retention_tracks_renames_dropped_slots_and_lazy_owned_bounds() {
    let catalog = Catalog::new();
    let budget = MemoryBudget::new(16384);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let retained = retain_value_with_control(
        control.copy_value(&source()).unwrap(),
        Some(&catalog),
        &control,
    )
    .unwrap();
    Arc::make_mut(&mut catalog.0.borrow_mut()).attributes[0].name = "renamed".into();
    let decoded = decode(&retained, &catalog, &control);
    let Value::Record(record) = &*decoded else {
        panic!("record")
    };
    assert_eq!(record.get(0).unwrap().0, "renamed");
    assert_eq!(
        *decode(record.get(0).unwrap().1, &catalog, &control),
        Value::Int(16_909_060)
    );
    drop(decoded);

    // Independently captured PostgreSQL in-bounds reads; #656 rejects the final out-of-bounds read.
    Arc::make_mut(&mut catalog.0.borrow_mut()).attributes[0].ty = ColumnType::BigInteger;
    let decoded = decode(&retained, &catalog, &control);
    let Value::Record(record) = &*decoded else {
        panic!("record")
    };
    assert_eq!(
        *decode(record.get(0).unwrap().1, &catalog, &control),
        Value::Int(361_984_551_007_945_476)
    );
    assert_eq!(
        *decode(record.get(1).unwrap().1, &catalog, &control),
        Value::Int(287_454_020)
    );
    let tail = record.get(2).unwrap().1;
    let copied =
        retain_value_with_control(control.copy_value(tail).unwrap(), None, &control).unwrap();
    let Value::Datum(datum) = &*copied else {
        panic!("deferred address")
    };
    assert_eq!(
        crate::expr::datums::read(datum).unwrap_err().sqlstate(),
        Some("XX001")
    );
    drop(copied);
    drop(decoded);

    {
        let mut next = catalog.0.borrow_mut();
        let next = Arc::make_mut(&mut next);
        next.attributes[0].ty = ColumnType::Integer;
        next.attributes.remove(1);
        next.dropped.push(DroppedCompositeAttribute::from_type(
            2,
            &ColumnType::Integer,
        ));
        next.attributes.push(CompositeAttribute {
            name: "tail".into(),
            ty: ColumnType::Text,
            collation_oid: None,
            number: 4,
        });
    }
    let decoded = decode(&retained, &catalog, &control);
    let Value::Record(record) = &*decoded else {
        panic!("record")
    };
    assert_eq!(record.descriptor().field_names(), ["renamed", "c", "tail"]);
    assert_eq!(
        *decode(record.get(1).unwrap().1, &catalog, &control),
        Value::Int(287_454_020)
    );
    assert_eq!(record.get(2).unwrap().1, &Value::Null);
    drop(decoded);
    drop(retained);
    assert_eq!(budget.used(), 0);
}

#[test]
fn nested_array_bounds_anonymous_metadata_and_rejected_retention_are_preserved() {
    let catalog = Catalog::new();
    let array = Value::Array(
        ArrayValue::with_lower_bounds(vec![source(), Value::Null], vec![-3])
            .unwrap()
            .with_element_type_oid(Some(20_001)),
    );
    let source = Value::Row(
        RowValue::typed(
            vec![array],
            vec![RecordFieldType {
                oid: 20_002,
                type_modifier: -1,
                collation_oid: Some(0),
            }],
        )
        .unwrap(),
    );
    for limit in [0, 64, 256, 1024, 16384] {
        let budget = MemoryBudget::new(limit);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&budget, &original, &invoking);
        let retained = control
            .copy_value(&source)
            .map_err(SQLError::from)
            .and_then(|value| retain_value_with_control(value, Some(&catalog), &control));
        if limit == 16384 {
            let retained = retained.unwrap();
            let Value::Row(row) = &*retained else {
                panic!("row")
            };
            assert_eq!(row.field_types().unwrap()[0].oid, 20_002);
            let Value::Array(array) = &row[0] else {
                panic!("array")
            };
            assert_eq!(array.lower_bounds(), [-3]);
            assert_eq!(array.dimensions(), [2]);
            assert_eq!(array.element_type_oid(), Some(20_001));
            assert!(matches!(array.elements(), [Value::Datum(_), Value::Null]));
        } else if let Err(error) = retained {
            assert_eq!(error.sqlstate(), Some("53200"));
        }
        assert_eq!(budget.used(), 0);
        for token in [&original, &invoking] {
            let value = control.copy_value(&Value::Null).unwrap();
            token.cancel();
            assert_eq!(
                retain_value_with_control(value, Some(&catalog), &control)
                    .unwrap_err()
                    .sqlstate(),
                Some("57014")
            );
            token.reset();
            assert_eq!(budget.used(), 0);
        }
    }
}
