//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    expr::{composites::CompositeTypeDescriptor, enums::EnumTypeLabels},
    ColumnType,
};
use std::sync::Arc;
use uqa_core::{memory::MemoryBudget, CancellationToken, DatumValue, RecordFieldType, RowValue};

struct Catalog(Option<Arc<CompositeTypeDescriptor>>);

impl SQLValueCatalog for Catalog {
    fn value_type_by_oid(&self, oid: u32) -> Result<Option<ColumnType>, SQLError> {
        if let Some(descriptor) = self
            .0
            .as_ref()
            .filter(|descriptor| descriptor.type_oid == oid)
        {
            return Ok(Some(ColumnType::Composite(
                crate::ast::CompositeTypeReference {
                    schema: "public".into(),
                    name: "image_record".into(),
                    oid,
                    array_oid: oid + 1,
                    relation_oid: descriptor.relation_oid,
                },
            )));
        }
        Ok(type_metadata::builtin_array_element(oid)
            .map(|element| ColumnType::Array(Box::new(element.clone()))))
    }
    fn value_composite_type(
        &self,
        oid: u32,
    ) -> Result<Option<Arc<CompositeTypeDescriptor>>, SQLError> {
        Ok(self
            .0
            .as_ref()
            .filter(|descriptor| descriptor.type_oid == oid)
            .cloned())
    }
    fn enum_type_labels(&self, _: u32) -> Result<Option<Arc<EnumTypeLabels>>, SQLError> {
        Ok(None)
    }
    fn enum_label_uncommitted(&self, _: u32) -> bool {
        false
    }
    fn enum_type_name(&self, _: u32) -> Result<Option<String>, SQLError> {
        Ok(None)
    }
    fn has_enum_types(&self) -> bool {
        false
    }
}

fn row(values: Vec<Value>, types: &[ColumnType]) -> Value {
    Value::Row(
        RowValue::typed(
            values,
            types
                .iter()
                .map(|ty| RecordFieldType {
                    oid: type_metadata::pg_type_oid(ty) as u32,
                    type_modifier: type_metadata::pg_type_modifier(ty) as i32,
                })
                .collect(),
        )
        .unwrap(),
    )
}

fn physical(value: &Value, ty: &ColumnType, catalog: Option<&dyn SQLValueCatalog>) -> Value {
    if matches!(value, Value::Null) {
        return Value::Null;
    }
    let composites = super::super::support::ValueCompositeCatalog(catalog);
    let encoded = datums::encoding::encode(
        value,
        ty,
        Some(&composites),
        &ProductionControl::uncontrolled(),
    )
    .unwrap();
    let mut bytes = Vec::new();
    if type_metadata::pg_type_len(ty) == -1 {
        bytes.extend_from_slice(&datums::encoding::header(encoded.len()).unwrap());
    }
    bytes.extend_from_slice(&encoded);
    Value::Datum(DatumValue::new(
        type_metadata::pg_type_oid(ty) as u32,
        0,
        bytes,
    ))
}

#[test]
fn record_image_comparison_matches_postgresql_for_logical_and_physical_fields() {
    let reference: serde_json::Value =
        serde_json::from_str(include_str!("postgresql.json")).unwrap();
    let memory = MemoryBudget::new(1024 * 1024);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    for case in reference["cases"].as_array().unwrap() {
        let name = case["type"].as_str().unwrap();
        let ty = ColumnType::from_sql_name(name).unwrap();
        let values = ["left", "right"].map(|field| {
            case[field].as_str().map_or(Value::Null, |text| {
                crate::expr::cast_value(&Value::Str(text.into()), name).unwrap()
            })
        });
        let expected = case["order"].as_i64().unwrap().cmp(&0);
        for left in [&values[0], &physical(&values[0], &ty, None)] {
            for right in [&values[1], &physical(&values[1], &ty, None)] {
                let left = row(vec![left.clone()], std::slice::from_ref(&ty));
                let right = row(vec![right.clone()], std::slice::from_ref(&ty));
                assert_eq!(
                    compare_record_images_with_catalog(
                        &left,
                        &right,
                        Some(&Catalog(None)),
                        &control
                    )
                    .unwrap(),
                    expected,
                    "{case}"
                );
                assert_eq!(
                    compare_record_images_with_catalog(
                        &right,
                        &left,
                        Some(&Catalog(None)),
                        &control
                    )
                    .unwrap(),
                    expected.reverse(),
                    "{case}"
                );
                assert_eq!(memory.used(), 0);
            }
        }
    }
}

#[test]
fn record_image_comparison_keeps_reached_type_and_byte_errors_lazy() {
    let broken = Value::Datum(DatumValue::new(25, 0, vec![0]));
    let types = [ColumnType::Integer, ColumnType::Text];
    let left = row(vec![Value::Int(1), broken.clone()], &types);
    let right = row(vec![Value::Int(2), broken.clone()], &types);
    let control = ProductionControl::uncontrolled();
    assert!(
        compare_record_images_with_catalog(&left, &right, None, &control)
            .unwrap()
            .is_lt()
    );
    let error = compare_record_images_with_catalog(&left, &left, None, &control).unwrap_err();
    assert_eq!(error.sqlstate(), Some("XX001"));
    let mismatched = row(
        vec![Value::Int(1), Value::Null],
        &[ColumnType::Integer, ColumnType::Json],
    );
    let error = compare_record_images_with_catalog(&left, &mismatched, None, &control).unwrap_err();
    assert_eq!(error.sqlstate(), Some("42804"));
    assert_eq!(
        error.to_string(),
        "cannot compare dissimilar column types text and json at record column 2"
    );
    let shorter = row(vec![Value::Int(1)], &[ColumnType::Integer]);
    let error = compare_record_images_with_catalog(&shorter, &left, None, &control).unwrap_err();
    assert_eq!(error.sqlstate(), Some("42804"));
}

#[test]
fn record_image_comparison_shares_the_callers_allowance_and_cancellation() {
    let ty = ColumnType::Numeric {
        precision: None,
        scale: None,
    };
    let value = row(
        vec![crate::expr::cast_value(&Value::Str("1.00".into()), "numeric").unwrap()],
        &[ty],
    );
    let memory = MemoryBudget::new(0);
    let statement = CancellationToken::new();
    let session = CancellationToken::new();
    let control = ProductionControl::new(&memory, &statement, &session);
    let error = compare_record_images_with_catalog(&value, &value, None, &control).unwrap_err();
    assert_eq!(error.sqlstate(), Some("53200"));
    assert_eq!(memory.used(), 0);
    for cancel_statement in [false, true] {
        let statement = CancellationToken::new();
        let session = CancellationToken::new();
        if cancel_statement {
            statement.cancel();
        } else {
            session.cancel();
        }
        let control = ProductionControl::new(&memory, &statement, &session);
        let error = compare_record_images_with_catalog(&value, &value, None, &control).unwrap_err();
        assert_eq!(error.sqlstate(), Some("57014"));
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn record_image_comparison_retains_named_tuple_field_images() {
    use crate::expr::composites::CompositeAttribute;
    let ty = ColumnType::Numeric {
        precision: None,
        scale: None,
    };
    let catalog = Catalog(Some(Arc::new(CompositeTypeDescriptor {
        type_oid: 80_001,
        relation_oid: 80_003,
        dropped: Vec::new(),
        attributes: vec![CompositeAttribute {
            name: "n".into(),
            ty,
            number: 1,
            collation_oid: None,
        }],
    })));
    let ty = catalog.value_type_by_oid(80_001).unwrap().unwrap();
    let values = ["1.0", "1.00"].map(|text| {
        Value::Record(uqa_core::RecordValue::from_parts(
            vec![(
                "n".into(),
                crate::expr::cast_value(&Value::Str(text.into()), "numeric").unwrap(),
            )],
            Some(80_001),
        ))
    });
    let memory = MemoryBudget::new(16 * 1024);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    for left in [&values[0], &physical(&values[0], &ty, Some(&catalog))] {
        for right in [&values[1], &physical(&values[1], &ty, Some(&catalog))] {
            assert_eq!(
                compare_record_images_with_catalog(left, right, Some(&catalog), &control).unwrap(),
                Ordering::Greater,
            );
            assert_eq!(
                compare_record_images_with_catalog(right, left, Some(&catalog), &control).unwrap(),
                Ordering::Less,
            );
            assert_eq!(memory.used(), 0);
        }
    }
}
