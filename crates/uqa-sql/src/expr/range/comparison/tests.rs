//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    ast::RangeSubtype,
    expr::range::{parse_multirange_with_control, parse_range_with_control},
};
use uqa_core::{memory::MemoryBudget, CancellationToken, Value};

#[derive(serde::Deserialize)]
struct Reference {
    postgresql_version: String,
    families: Vec<Family>,
}

#[derive(serde::Deserialize)]
struct Family {
    kind: String,
    values: Vec<String>,
    comparisons: Vec<Vec<i8>>,
}

fn subtype(kind: &str) -> RangeSubtype {
    match kind {
        "int4range" | "int4multirange" => RangeSubtype::Integer,
        "int8range" | "int8multirange" => RangeSubtype::BigInteger,
        "numrange" | "nummultirange" => RangeSubtype::Numeric,
        "daterange" | "datemultirange" => RangeSubtype::Date,
        "tsrange" | "tsmultirange" => RangeSubtype::Timestamp,
        "tstzrange" | "tstzmultirange" => RangeSubtype::TimestampTz,
        _ => panic!("unknown PostgreSQL range family"),
    }
}

#[test]
fn range_and_multirange_order_matches_postgresql_bound_semantics() {
    let reference: Reference = serde_json::from_str(include_str!("postgresql.json")).unwrap();
    assert!(reference.postgresql_version.starts_with("PostgreSQL 18."));
    let memory = MemoryBudget::new(1024 * 1024);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let mut observations = 0;
    for family in reference.families {
        for (left_index, left) in family.values.iter().enumerate() {
            for (right_index, right) in family.values.iter().enumerate() {
                let actual = if family.kind.contains("multirange") {
                    let left = parse_multirange_with_control(left, subtype(&family.kind), &control)
                        .unwrap();
                    let right =
                        parse_multirange_with_control(right, subtype(&family.kind), &control)
                            .unwrap();
                    let retained = memory.used();
                    let order = left.cmp_with_control(&right, &control).unwrap();
                    assert_eq!(memory.used(), retained);
                    order
                } else {
                    let left =
                        parse_range_with_control(left, subtype(&family.kind), &control).unwrap();
                    let right =
                        parse_range_with_control(right, subtype(&family.kind), &control).unwrap();
                    let retained = memory.used();
                    let order = left.cmp_with_control(&right, &control).unwrap();
                    assert_eq!(memory.used(), retained);
                    order
                };
                assert_eq!(
                    actual,
                    family.comparisons[left_index][right_index].cmp(&0),
                    "{}: {left} versus {right}",
                    family.kind,
                );
                assert_eq!(memory.used(), 0);
                observations += 1;
            }
        }
    }
    assert_eq!(observations, 1264);
}

#[test]
fn admitted_range_comparison_preserves_zero_copy_and_cancellation_boundaries() {
    let ordinary = ProductionControl::uncontrolled();
    let left = parse_range_with_control("[1,3)", RangeSubtype::Integer, &ordinary).unwrap();
    let right = parse_range_with_control("[2,3)", RangeSubtype::Integer, &ordinary).unwrap();
    let multi_left =
        parse_multirange_with_control("{[1,3),[5,7)}", RangeSubtype::Integer, &ordinary).unwrap();
    let multi_right =
        parse_multirange_with_control("{[1,3),[5,7),[10,11)}", RangeSubtype::Integer, &ordinary)
            .unwrap();
    let memory = MemoryBudget::new(0);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    assert_eq!(
        left.cmp_with_control(&right, &control).unwrap(),
        Ordering::Less
    );
    assert_eq!(
        multi_left.cmp_with_control(&multi_right, &control).unwrap(),
        Ordering::Less
    );
    assert_eq!(memory.peak(), 0);
    token.cancel();
    for error in [
        left.cmp_with_control(&right, &control).unwrap_err(),
        multi_left
            .cmp_with_control(&multi_right, &control)
            .unwrap_err(),
    ] {
        assert_eq!(error.sqlstate(), Some("57014"));
    }
    assert_eq!(memory.used(), 0);
}

struct FieldCatalog(std::sync::Arc<crate::expr::composites::CompositeTypeDescriptor>);

impl crate::expr::SQLValueCatalog for FieldCatalog {
    fn enum_label_uncommitted(&self, _: u32) -> bool {
        false
    }
    fn enum_type_name(&self, _: u32) -> Result<Option<String>> {
        Ok(None)
    }
    fn has_enum_types(&self) -> bool {
        false
    }
    fn enum_type_labels(
        &self,
        _: u32,
    ) -> Result<Option<std::sync::Arc<crate::expr::enums::EnumTypeLabels>>> {
        Ok(None)
    }

    fn value_composite_type(
        &self,
        oid: u32,
    ) -> Result<Option<std::sync::Arc<crate::expr::composites::CompositeTypeDescriptor>>> {
        Ok((oid == self.0.type_oid).then(|| self.0.clone()))
    }
}

fn typed_containers(text: &str, oid: u32) -> Vec<Value> {
    let row = Value::Row(
        uqa_core::RowValue::typed(
            vec![Value::Str(text.into())],
            vec![uqa_core::RecordFieldType {
                collation_oid: None,
                oid,
                type_modifier: -1,
            }],
        )
        .unwrap(),
    );
    vec![
        row.clone(),
        Value::Record(
            uqa_core::RecordValue::from(vec![("value".into(), Value::Str(text.into()))])
                .with_type_oid(Some(16_384)),
        ),
        Value::Array(
            uqa_core::ArrayValue::try_new(vec![Value::Str(text.into())])
                .unwrap()
                .with_element_type_oid(Some(oid)),
        ),
        Value::Row(
            uqa_core::RowValue::typed(
                vec![row],
                vec![uqa_core::RecordFieldType {
                    collation_oid: None,
                    oid: 2249,
                    type_modifier: -1,
                }],
            )
            .unwrap(),
        ),
    ]
}

#[test]
fn selected_range_fields_preserve_postgresql_order_and_equality_in_nested_containers() {
    use crate::expr::{
        compare_typed_values_with_enum_catalog, equal_typed_values_with_enum_catalog,
    };
    let reference: Reference = serde_json::from_str(include_str!("postgresql.json")).unwrap();
    let memory = MemoryBudget::new(1024 * 1024);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    for family in reference.families {
        let ty = crate::ColumnType::from_sql_name(&family.kind).unwrap();
        let oid = crate::catalog::type_metadata::pg_type_oid(&ty) as u32;
        let catalog = FieldCatalog(std::sync::Arc::new(
            crate::expr::composites::CompositeTypeDescriptor {
                type_oid: 16_384,
                relation_oid: 16_385,
                dropped: vec![],
                attributes: vec![crate::expr::composites::CompositeAttribute {
                    name: "value".into(),
                    ty,
                    collation_oid: None,
                    number: 1,
                }],
            },
        ));
        for (i, left) in family.values.iter().enumerate() {
            for (j, right) in family.values.iter().enumerate() {
                let expected = family.comparisons[i][j].cmp(&0);
                for (left, right) in typed_containers(left, oid)
                    .iter()
                    .zip(typed_containers(right, oid))
                {
                    assert_eq!(
                        compare_typed_values_with_enum_catalog(
                            left,
                            &right,
                            &control,
                            Some(&catalog),
                            None
                        )
                        .unwrap(),
                        expected,
                        "{}: {left:?} versus {right:?}",
                        family.kind
                    );
                    assert_eq!(
                        equal_typed_values_with_enum_catalog(
                            left,
                            &right,
                            &control,
                            Some(&catalog)
                        )
                        .unwrap(),
                        expected.is_eq()
                    );
                    assert_eq!(memory.used(), 0);
                }
                let left = typed_containers(left, oid).remove(0);
                let right = typed_containers(right, oid).remove(0);
                for (op, expected) in [
                    (crate::ast::BinaryOp::Equal, expected.is_eq()),
                    (crate::ast::BinaryOp::Less, expected.is_lt()),
                ] {
                    assert_eq!(
                        crate::expr::eval_comparison_truth_with_control(
                            op, &left, &right, &control
                        )
                        .unwrap(),
                        Some(expected)
                    );
                }
                assert_eq!(memory.used(), 0);
            }
        }
    }
}

#[test]
fn selected_range_fields_keep_text_semantics_and_release_failed_parse_reservations() {
    use crate::expr::compare_typed_values_with_control;
    let ordinary = ProductionControl::uncontrolled();
    assert!(compare_typed_values_with_control(
        &Value::Str("empty".into()),
        &Value::Str("[1,3)".into()),
        &ordinary
    )
    .unwrap()
    .is_gt());
    let left = typed_containers("[1.0,3.00)", 3906).remove(0);
    let right = typed_containers("[1.00,3.0)", 3906).remove(0);
    let memory = MemoryBudget::new(0);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let error = compare_typed_values_with_control(&left, &right, &control).unwrap_err();
    assert_eq!(error.sqlstate(), Some("53200"));
    assert_eq!(memory.used(), 0);
    token.cancel();
    let error = compare_typed_values_with_control(&left, &right, &control).unwrap_err();
    assert_eq!(error.sqlstate(), Some("57014"));
    assert_eq!(memory.used(), 0);
}
