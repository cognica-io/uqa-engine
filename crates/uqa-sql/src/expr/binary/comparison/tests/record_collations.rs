//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::super::*;
use crate::{
    expr::{
        composites::{CompositeAttribute, CompositeTypeDescriptor},
        enums::EnumTypeLabels,
        SQLValueCatalog,
    },
    ColumnType,
};
use std::sync::Arc;
use uqa_core::{
    memory::MemoryBudget, ArrayValue, CancellationToken, RecordFieldType, RecordValue, RowValue,
};

fn row(text: &str, collation_oid: u32) -> Value {
    Value::Row(
        RowValue::typed(
            vec![Value::Str(text.into())],
            vec![RecordFieldType {
                oid: 25,
                type_modifier: -1,
                collation_oid: Some(collation_oid),
            }],
        )
        .unwrap(),
    )
}

struct Catalog(Vec<Arc<CompositeTypeDescriptor>>);

impl Catalog {
    fn new() -> Self {
        Self(
            [
                (90_001, ColumnType::Text, 950),
                (90_002, ColumnType::Text, 963),
                (90_003, composite_type(90_001), 0),
                (90_004, composite_type(90_002), 0),
                (90_005, ColumnType::Array(Box::new(ColumnType::Text)), 963),
            ]
            .into_iter()
            .map(|(type_oid, ty, collation)| {
                Arc::new(CompositeTypeDescriptor {
                    type_oid,
                    relation_oid: type_oid + 10,
                    dropped: vec![],
                    attributes: vec![CompositeAttribute {
                        name: "v".into(),
                        number: 1,
                        ty,
                        collation_oid: Some(collation),
                    }],
                })
            })
            .collect(),
        )
    }
}

impl SQLValueCatalog for Catalog {
    fn value_type_by_oid(&self, oid: u32) -> Result<Option<ColumnType>> {
        if self.0.iter().any(|descriptor| descriptor.type_oid == oid) {
            return Ok(Some(composite_type(oid)));
        }
        if [90_101, 90_102].contains(&oid) {
            return Ok(Some(domain_type(oid)));
        }
        Ok(crate::catalog::type_metadata::builtin_array_element(oid)
            .map(|element| ColumnType::Array(Box::new(element.clone()))))
    }

    fn value_composite_type(&self, oid: u32) -> Result<Option<Arc<CompositeTypeDescriptor>>> {
        Ok(self
            .0
            .iter()
            .find(|descriptor| descriptor.type_oid == oid)
            .cloned())
    }
    fn enum_type_labels(&self, _: u32) -> Result<Option<Arc<EnumTypeLabels>>> {
        Ok(None)
    }
    fn enum_label_uncommitted(&self, _: u32) -> bool {
        false
    }
    fn enum_type_name(&self, _: u32) -> Result<Option<String>> {
        Ok(None)
    }
    fn has_enum_types(&self) -> bool {
        false
    }
}

fn typed_row(values: Vec<Value>, fields: &[(u32, Option<u32>)]) -> Value {
    Value::Row(
        RowValue::typed(
            values,
            fields
                .iter()
                .map(|(oid, collation_oid)| RecordFieldType {
                    oid: *oid,
                    type_modifier: -1,
                    collation_oid: *collation_oid,
                })
                .collect(),
        )
        .unwrap(),
    )
}

fn named(text: &str, oid: u32) -> Value {
    Value::Record(RecordValue::from_parts(
        vec![("v".into(), Value::Str(text.into()))],
        Some(oid),
    ))
}

fn array(text: Option<&str>) -> Value {
    Value::Array(
        ArrayValue::try_new(text.into_iter().map(|v| Value::Str(v.into())).collect())
            .unwrap()
            .with_element_type_oid(Some(25)),
    )
}

fn operands(suffix: &str) -> (Value, Value) {
    let mismatch = || (row("a", 950), row("b", 963));
    match suffix {
        "equal_text_mismatch" => (row("a", 950), row("a", 963)),
        "unequal_text_mismatch" => mismatch(),
        "null_text_mismatch" => (
            typed_row(vec![Value::Null], &[(25, Some(950))]),
            typed_row(vec![Value::Null], &[(25, Some(963))]),
        ),
        "one_null_mismatch" => (
            typed_row(vec![Value::Null], &[(25, Some(950))]),
            row("a", 963),
        ),
        "deciding_prefix" | "reached_mismatch" => {
            let right_prefix = if suffix == "deciding_prefix" { 2 } else { 1 };
            (
                typed_row(
                    vec![Value::Int(1), Value::Str("a".into())],
                    &[(23, Some(0)), (25, Some(950))],
                ),
                typed_row(
                    vec![Value::Int(right_prefix), Value::Str("b".into())],
                    &[(23, Some(0)), (25, Some(963))],
                ),
            )
        }
        "named_mismatch" => (named("B", 90_001), named("a", 90_002)),
        "named_unicode" => (named("B", 90_002), named("a", 90_002)),
        "named_c" => (named("B", 90_001), named("a", 90_001)),
        "array_field_mismatch" | "empty_array_mismatch" => {
            let populated = suffix == "array_field_mismatch";
            (
                typed_row(vec![array(populated.then_some("a"))], &[(1009, Some(950))]),
                typed_row(vec![array(populated.then_some("b"))], &[(1009, Some(963))]),
            )
        }
        other => panic!("uncaptured record input {other}"),
    }
}

#[test]
fn record_collation_matches_all_postgresql_lazy_field_cases() {
    let reference: serde_json::Value =
        serde_json::from_str(include_str!("record_collations.json")).unwrap();
    let memory = MemoryBudget::new(1024 * 1024);
    let token = CancellationToken::new();
    let catalog = Catalog::new();
    for control in [
        ProductionControl::uncontrolled(),
        ProductionControl::new(&memory, &token, &token),
    ] {
        for case in reference["cases"].as_array().unwrap() {
            let name = case["name"].as_str().unwrap();
            let ordering = name.starts_with("btrecordcmp_");
            let suffix = name
                .strip_prefix("btrecordcmp_")
                .or_else(|| name.strip_prefix("record_eq_"))
                .unwrap();
            let (left, right) = operands(suffix);
            let result = if ordering {
                compare_typed_values_with_enum_catalog(
                    &left,
                    &right,
                    &control,
                    Some(&catalog),
                    None,
                )
                .map(|v| match v {
                    Ordering::Less => "-1",
                    Ordering::Equal => "0",
                    Ordering::Greater => "1",
                })
            } else {
                equal_typed_values_with_enum_catalog(&left, &right, &control, Some(&catalog))
                    .map(|v| if v { "t" } else { "f" })
            };
            match result {
                Ok(value) => assert_eq!(case["output"].as_str(), Some(value), "{name}"),
                Err(error) => {
                    assert_eq!(error.sqlstate(), case["sqlstate"].as_str(), "{name}");
                    assert_eq!(
                        error.to_string(),
                        case["message"].as_str().unwrap(),
                        "{name}"
                    );
                    assert_eq!(error.hint(), case["hint"].as_str(), "{name}");
                }
            }
            assert_eq!(memory.used(), 0, "{name}");
        }
    }
    assert_eq!(reference["cases"].as_array().unwrap().len(), 22);
}

#[test]
fn selected_collation_reaches_array_elements_and_preserves_empty_and_null_shortcuts() {
    let control = ProductionControl::uncontrolled();
    let left = array(Some("B"));
    let right = array(Some("a"));
    assert!(
        compare_typed_values_with_catalog_and_collation(&left, &right, 963, &control, None)
            .unwrap()
            .is_gt()
    );
    assert!(
        compare_typed_values_with_catalog_and_collation(&left, &right, 950, &control, None)
            .unwrap()
            .is_lt()
    );
    assert_eq!(
        compare_typed_values_with_catalog_and_collation(&left, &right, 0, &control, None)
            .unwrap_err()
            .sqlstate(),
        Some("42P22")
    );
    assert!(equal_typed_values_with_catalog_and_collation(
        &array(None),
        &array(None),
        0,
        &control,
        None
    )
    .unwrap());
    assert!(compare_typed_values_with_catalog_and_collation(
        &Value::Null,
        &Value::Null,
        0,
        &control,
        None
    )
    .unwrap()
    .is_eq());
    let legacy = typed_row(vec![Value::Str("a".into())], &[(25, None)]);
    assert!(equal_typed_values_with_enum_catalog(&legacy, &row("a", 100), &control, None).unwrap());
    let invalid = row("a", 0);
    assert_eq!(
        equal_typed_values_with_enum_catalog(&invalid, &invalid, &control, None)
            .unwrap_err()
            .sqlstate(),
        Some("42P22")
    );
}

#[test]
fn selected_collation_keeps_equality_bounded_and_propagates_quota_and_both_cancellations() {
    let text = "a".to_owned() + &"\u{301}\u{323}".repeat(512);
    let left = row(&text, 963);
    let right = row(&(text.clone() + "b"), 963);
    let memory = MemoryBudget::new(0);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&memory, &original, &invoking);
    assert!(equal_typed_values_with_enum_catalog(&left, &left, &control, None).unwrap());
    assert!(!equal_typed_values_with_enum_catalog(&left, &right, &control, None).unwrap());
    assert_eq!(
        compare_typed_values_with_control(&left, &right, &control)
            .unwrap_err()
            .sqlstate(),
        Some("53200")
    );
    assert_eq!(memory.used(), 0);
    for original_scope in [true, false] {
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&memory, &original, &invoking);
        if original_scope {
            original.cancel();
        } else {
            invoking.cancel();
        }
        assert_eq!(
            compare_typed_values_with_control(&left, &right, &control)
                .unwrap_err()
                .sqlstate(),
            Some("57014")
        );
        assert_eq!(
            equal_typed_values_with_enum_catalog(&left, &right, &control, None)
                .unwrap_err()
                .sqlstate(),
            Some("57014")
        );
        assert_eq!(memory.used(), 0);
    }
}

fn composite_type(oid: u32) -> ColumnType {
    ColumnType::Composite(crate::ast::CompositeTypeReference {
        schema: "public".into(),
        name: format!("collation_record_{oid}"),
        oid,
        array_oid: oid + 1000,
        relation_oid: oid + 10,
    })
}

fn domain_type(oid: u32) -> ColumnType {
    ColumnType::Domain {
        schema: "public".into(),
        name: format!("collation_domain_{oid}"),
        oid,
        array_oid: Some(oid + 1000),
        collation_oid: Some(if oid == 90_101 { 950 } else { 963 }),
        base: Box::new(ColumnType::Text),
    }
}

fn nested_operands(name: &str, text: &str) -> (Value, ColumnType, u32) {
    let collation = if name.ends_with("unicode") {
        963
    } else if name.ends_with("pg_unicode_fast") {
        6411
    } else {
        950
    };
    if name.starts_with("scalar_") {
        return (Value::Str(text.into()), ColumnType::Text, collation);
    }
    if name.starts_with("domain_array_") {
        let oid = if collation == 950 { 90_101 } else { 90_102 };
        let value = Value::Array(
            ArrayValue::try_new(vec![Value::Str(text.into())])
                .unwrap()
                .with_element_type_oid(Some(oid)),
        );
        return (
            value,
            ColumnType::Array(Box::new(domain_type(oid))),
            collation,
        );
    }
    if name.starts_with("array_") || name.starts_with("nested_array_") {
        let value = if name.starts_with("nested_array_") {
            Value::Array(
                ArrayValue::try_new(vec![array(Some(text))])
                    .unwrap()
                    .with_element_type_oid(Some(25)),
            )
        } else {
            array(Some(text))
        };
        return (
            value,
            ColumnType::Array(Box::new(ColumnType::Text)),
            collation,
        );
    }
    if name.starts_with("anonymous_record_") {
        return (row(text, collation), ColumnType::Record, 0);
    }
    let oid = if collation == 950 { 90_001 } else { 90_002 };
    let value = if name.starts_with("nested_record_") {
        Value::Record(RecordValue::from_parts(
            vec![("v".into(), named(text, oid))],
            Some(oid + 2),
        ))
    } else if name == "record_array_field_unicode" {
        Value::Record(RecordValue::from_parts(
            vec![("v".into(), array(Some(text)))],
            Some(90_005),
        ))
    } else {
        named(text, oid)
    };
    (value, ColumnType::Record, 0)
}

#[test]
fn selected_btree_collation_matches_postgresql_nested_domains_arrays_and_records() {
    let reference: serde_json::Value =
        serde_json::from_str(include_str!("nested_collations.json")).unwrap();
    let memory = MemoryBudget::new(1024 * 1024);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let catalog = Catalog::new();
    for case in reference["cases"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let (left, ty, collation) = nested_operands(name, "B");
        let (right, _, _) = nested_operands(name, "a");
        let class =
            crate::catalog::index::operator_classes::default_btree_operator_class(&ty).unwrap();
        let binding = class.bind_input(&ty, class.name, &control).unwrap();
        let actual = crate::expr::compare_btree_inputs_with_catalog_and_control(
            binding,
            &left,
            &right,
            collation,
            Some(&catalog),
            &control,
        )
        .unwrap();
        assert_eq!(actual, case["order"].as_i64().unwrap().cmp(&0), "{name}");
        assert_eq!(memory.used(), 0, "{name}");
    }
    assert_eq!(reference["cases"].as_array().unwrap().len(), 19);
}

fn physical_record(value: &Value, ty: &ColumnType, catalog: &Catalog) -> Value {
    let fields = super::super::support::ValueCompositeCatalog(Some(catalog));
    let encoded = crate::expr::datums::encoding::encode(
        value,
        ty,
        Some(&fields),
        &ProductionControl::uncontrolled(),
    )
    .unwrap();
    let mut bytes = vec![0xee];
    bytes.extend_from_slice(&crate::expr::datums::encoding::header(encoded.len()).unwrap());
    bytes.extend_from_slice(&encoded);
    Value::Datum(uqa_core::DatumValue::new(
        crate::catalog::type_metadata::pg_type_oid(ty) as u32,
        1,
        bytes,
    ))
}

#[test]
fn physical_record_collation_reuses_selected_descriptors_and_preserves_diagnostics() {
    let reference: serde_json::Value =
        serde_json::from_str(include_str!("record_collations.json")).unwrap();
    let catalog = Catalog::new();
    let memory = MemoryBudget::new(1024 * 1024);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let mut observations = 0;
    for case in reference["cases"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|case| case["name"].as_str().unwrap().contains("_named_"))
    {
        let name = case["name"].as_str().unwrap();
        let ordering = name.starts_with("btrecordcmp_");
        let suffix = name
            .strip_prefix("btrecordcmp_")
            .or_else(|| name.strip_prefix("record_eq_"))
            .unwrap();
        let (left, right) = operands(suffix);
        let Value::Record(left_record) = &left else {
            panic!("named left")
        };
        let Value::Record(right_record) = &right else {
            panic!("named right")
        };
        let physical_left = physical_record(
            &left,
            &composite_type(left_record.type_oid().unwrap()),
            &catalog,
        );
        let physical_right = physical_record(
            &right,
            &composite_type(right_record.type_oid().unwrap()),
            &catalog,
        );
        for left in [&left, &physical_left] {
            for right in [&right, &physical_right] {
                let result = if ordering {
                    compare_typed_values_with_enum_catalog(
                        left,
                        right,
                        &control,
                        Some(&catalog),
                        None,
                    )
                    .map(|v| match v {
                        Ordering::Less => "-1",
                        Ordering::Equal => "0",
                        Ordering::Greater => "1",
                    })
                } else {
                    equal_typed_values_with_enum_catalog(left, right, &control, Some(&catalog))
                        .map(|v| if v { "t" } else { "f" })
                };
                match result {
                    Ok(value) => assert_eq!(case["output"].as_str(), Some(value), "{name}"),
                    Err(error) => {
                        assert_eq!(error.sqlstate(), case["sqlstate"].as_str(), "{name}");
                        assert_eq!(
                            error.to_string(),
                            case["message"].as_str().unwrap(),
                            "{name}"
                        );
                        assert_eq!(error.hint(), case["hint"].as_str(), "{name}");
                    }
                }
                assert_eq!(memory.used(), 0, "{name}");
                observations += 1;
            }
        }
    }
    assert_eq!(observations, 24);
}
