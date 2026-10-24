//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::expr::enums::{EnumTypeLabel, EnumTypeLabels};
use crate::{expr::observations, ResultRow};
use std::sync::Arc;
use uqa_core::{ArrayValue, DatumValue, EnumLabelKey, EnumValue, RecordFieldType, RowValue};

struct Catalog;

fn label() -> EnumValue {
    EnumValue::new(16_384, EnumLabelKey::from_bytes(vec![0x80]).unwrap())
        .with_label_oid(Some(16_386))
}

impl EnumLabelCatalog for Catalog {
    fn enum_type_labels(&self, oid: u32) -> Result<Option<Arc<EnumTypeLabels>>, SQLError> {
        Ok((oid == 16_384).then(|| {
            Arc::new(EnumTypeLabels {
                type_oid: oid,
                labels: vec![EnumTypeLabel {
                    oid: 16_386,
                    key: label().key().clone(),
                    label: "current".into(),
                }],
            })
        }))
    }

    fn enum_value_by_oid(&self, oid: u32) -> Result<Option<EnumValue>, SQLError> {
        Ok((oid == 16_386).then(label))
    }

    fn enum_label_uncommitted(&self, _: u32) -> bool {
        false
    }

    fn enum_type_name(&self, oid: u32) -> Result<Option<String>, SQLError> {
        Ok((oid == 16_384).then(|| "color".into()))
    }

    fn has_enum_types(&self) -> bool {
        true
    }
}

#[test]
fn ordinary_result_values_are_inspected_once() {
    let rows = (0..8192)
        .map(|id| {
            ResultRow::from([
                ("id".into(), Value::Int(id)),
                ("amount".into(), Value::Int(id * 7)),
            ])
        })
        .collect();
    let mut result = SQLResult::from_rows(vec!["id".into(), "amount".into()], rows);
    let before = observations::visits();
    render_result_enum_labels(None, &mut result).unwrap();
    let inspected = observations::visits() - before;
    for (id, row) in result.rows.iter().enumerate() {
        let id = i64::try_from(id).unwrap();
        assert_eq!(row["id"], Value::Int(id));
        assert_eq!(row["amount"], Value::Int(id * 7));
    }
    assert_eq!(inspected, 8192 * 2, "one carrier inspection per scalar");
}

#[test]
fn ordinary_nested_output_retains_payloads_with_one_carrier_walk() {
    let values = (0..4096).map(Value::Int).collect::<Vec<_>>();
    let pointer = values.as_ptr();
    let mut result = SQLResult::from_rows(
        vec!["values".into()],
        vec![ResultRow::from([("values".into(), Value::List(values))])],
    );
    let before = observations::visits();
    let allocation = allocation_counter::measure(|| {
        render_result_enum_labels(None, &mut result).unwrap();
    });
    let inspected = observations::visits() - before;
    let Value::List(values) = &result.rows[0]["values"] else {
        panic!("list output must retain its carrier");
    };
    assert_eq!(values.as_ptr(), pointer);
    assert_eq!(values.len(), 4096);
    for (index, value) in values.iter().enumerate() {
        assert_eq!(*value, Value::Int(i64::try_from(index).unwrap()));
    }
    assert_eq!(allocation.count_total, 0);
    assert_eq!(
        inspected, 4097,
        "inspect the container and every child once"
    );
}

fn nested_carrier_input(field_types: &[RecordFieldType]) -> Value {
    Value::Record(
        vec![
            (
                "row".into(),
                Value::Row(
                    RowValue::typed(
                        vec![
                            Value::Datum(DatumValue::new(23, 0, 7_i32.to_le_bytes().to_vec())),
                            Value::Enum(label()),
                        ],
                        field_types.to_vec(),
                    )
                    .unwrap(),
                ),
            ),
            (
                "array".into(),
                Value::Array(
                    ArrayValue::with_lower_bounds(
                        vec![Value::Enum(label()), Value::Null],
                        vec![-3],
                    )
                    .unwrap()
                    .with_element_type_oid(Some(16_384)),
                ),
            ),
            (
                "map".into(),
                Value::Map(ResultRow::from([(
                    "list".into(),
                    Value::List(vec![Value::Enum(label()), Value::Null]),
                )])),
            ),
        ]
        .into(),
    )
}

#[test]
fn named_and_positional_output_preserve_nested_metadata_and_decoded_enums() {
    let field_types = vec![
        RecordFieldType {
            oid: 23,
            type_modifier: -1,
            collation_oid: None,
        },
        RecordFieldType {
            oid: 16_384,
            type_modifier: -1,
            collation_oid: None,
        },
    ];
    let input = nested_carrier_input(&field_types);
    let encoded_enum = Value::Datum(DatumValue::new(
        16_384,
        0,
        16_386_u32.to_le_bytes().to_vec(),
    ));
    let mut result = SQLResult::from_rows_with_positions(
        vec!["item".into(), "item".into()],
        vec![ResultRow::new()],
        Some(vec![vec![input, encoded_enum]]),
    );
    render_result_enum_labels(Some(&Catalog), &mut result).unwrap();
    let expected = Value::Record(
        vec![
            (
                "row".into(),
                Value::Row(
                    RowValue::typed(
                        vec![Value::Int(7), Value::Str("current".into())],
                        field_types.clone(),
                    )
                    .unwrap(),
                ),
            ),
            (
                "array".into(),
                Value::Array(
                    ArrayValue::with_lower_bounds(
                        vec![Value::Str("current".into()), Value::Null],
                        vec![-3],
                    )
                    .unwrap()
                    .with_element_type_oid(Some(16_384)),
                ),
            ),
            (
                "map".into(),
                Value::Map(ResultRow::from([(
                    "list".into(),
                    Value::List(vec![Value::Str("current".into()), Value::Null]),
                )])),
            ),
        ]
        .into(),
    );
    assert_eq!(result.rows[0]["item"], expected);
    assert_eq!(result.rows[0]["item_1"], Value::Str("current".into()));
    assert_eq!(
        result.positional_rows,
        Some(vec![vec![expected, Value::Str("current".into())]])
    );
    let Value::Record(fields) = &result.rows[0]["item"] else {
        panic!("record output")
    };
    let Value::Row(row) = &fields.values()[0] else {
        panic!("row output")
    };
    assert_eq!(row.field_types(), Some(field_types.as_slice()));
    let Value::Array(array) = &fields.values()[1] else {
        panic!("array output")
    };
    assert_eq!(array.lower_bounds(), &[-3]);
    assert_eq!(array.element_type_oid(), Some(16_384));
}

#[test]
fn datum_failure_precedes_enum_lookup_and_preserves_the_value() {
    let value = Value::List(vec![
        Value::Enum(label()),
        Value::Datum(DatumValue::new(23, 0, vec![])),
    ]);
    let original = value.clone();
    let mut result = SQLResult::from_rows(
        vec!["value".into()],
        vec![ResultRow::from([("value".into(), value)])],
    );
    let error = render_result_enum_labels(None, &mut result).unwrap_err();
    assert_eq!(error.sqlstate(), Some("XX001"));
    assert!(result.rows[0]["value"].has_same_representation(&original));
}

#[test]
fn enum_failure_keeps_the_completed_datum_phase() {
    let value = Value::List(vec![
        Value::Enum(label()),
        Value::Datum(DatumValue::new(23, 0, 7_i32.to_le_bytes().to_vec())),
    ]);
    let mut result = SQLResult::from_rows(
        vec!["value".into()],
        vec![ResultRow::from([("value".into(), value)])],
    );
    let error = render_result_enum_labels(None, &mut result).unwrap_err();
    assert_eq!(
        error.to_string(),
        "internal error: enum type OID 16384 is not available in the statement catalog"
    );
    assert_eq!(
        result.rows[0]["value"],
        Value::List(vec![Value::Enum(label()), Value::Int(7)])
    );
}

#[test]
fn failed_datum_does_not_inspect_later_values() {
    let value = Value::List(vec![
        Value::Datum(DatumValue::new(23, 0, vec![])),
        Value::List((0..4096).map(Value::Int).collect()),
    ]);
    let mut result = SQLResult::from_rows(
        vec!["value".into()],
        vec![ResultRow::from([("value".into(), value)])],
    );
    let before = observations::visits();
    let error = render_result_enum_labels(None, &mut result).unwrap_err();
    assert_eq!(error.sqlstate(), Some("XX001"));
    assert_eq!(observations::visits() - before, 2);
}
