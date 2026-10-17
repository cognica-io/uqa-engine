//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::{node_tree, type_metadata};
use uqa_core::{memory::MemoryBudget, CancellationToken, DatumValue};

mod production;

struct Names;

impl node_tree::deparse::ExpressionNames for Names {
    fn column(&self, _: i64) -> Result<String, SQLError> {
        panic!("constant has no column")
    }
    fn routine(&self, _: i64) -> Result<Vec<String>, SQLError> {
        panic!("constant has no routine")
    }
    fn type_name(&self, oid: i64, _: i64) -> Result<String, SQLError> {
        Ok(column_type(oid as u32).sql_name())
    }
}

fn column_type(oid: u32) -> ColumnType {
    type_metadata::builtin_scalar_type(oid)
        .cloned()
        .unwrap_or_else(|| {
            ColumnType::Array(Box::new(
                type_metadata::builtin_array_element(oid).unwrap().clone(),
            ))
        })
}

fn fixtures() -> Vec<serde_json::Value> {
    let data: serde_json::Value = serde_json::from_str(include_str!("fixtures.json")).unwrap();
    data["cases"].as_array().unwrap().clone()
}

fn physical(case: &serde_json::Value) -> DatumValue {
    let node_tree::Field::Node(node) = node_tree::parse(case["node"].as_str().unwrap()).unwrap()
    else {
        panic!("constant node")
    };
    let node_tree::Field::Datum { bytes, .. } = node.field("constvalue").unwrap() else {
        panic!("constant datum")
    };
    DatumValue::new(case["oid"].as_u64().unwrap() as u32, 0, bytes.clone())
}

#[test]
fn native_range_datums_match_postgresql_bytes_output_and_catalog_deparse() {
    for case in fixtures() {
        let oid = case["oid"].as_u64().unwrap() as u32;
        let ty = column_type(oid);
        let input = crate::expr::cast_value(
            &Value::Str(case["input"].as_str().unwrap().into()),
            &ty.sql_name(),
        )
        .unwrap();
        let node = node_tree::constant(&input, &ty, None, None).unwrap();
        let expected = node_tree::parse(case["node"].as_str().unwrap()).unwrap();
        assert_eq!(node_tree::Field::Node(node), expected, "{}", case["id"]);
        let retained = physical(&case);
        let decoded = crate::expr::datums::read(&retained).unwrap();
        assert_eq!(
            crate::result::format_postgres_text(&decoded, &ty, None).unwrap(),
            case["output"].as_str().unwrap(),
            "{}",
            case["id"]
        );
        if retained.bytes().len() - 4 < 127 {
            let payload = &retained.bytes()[4..];
            let mut short = vec![0xff, ((payload.len() as u8 + 1) << 1) | 1];
            short.extend_from_slice(payload);
            assert_eq!(
                crate::expr::datums::read(&DatumValue::new(oid, 1, short)).unwrap(),
                decoded,
                "{} short retained field",
                case["id"]
            );
        }
        for (pretty, key) in [(false, "sql"), (true, "pretty")] {
            assert_eq!(
                node_tree::deparse::expression(&expected, &Names, pretty).unwrap(),
                case[key].as_str().unwrap(),
                "{}",
                case["id"]
            );
        }
        let copied =
            crate::expr::datums::copy_constant_field(&Value::Datum(retained), &ty).unwrap();
        assert_eq!(
            node_tree::Field::Node(node_tree::constant(&copied, &ty, None, None).unwrap()),
            expected,
            "{}",
            case["id"]
        );
    }
}

#[test]
fn native_range_datum_reads_preserve_the_original_allowance_and_both_cancellations() {
    for case in fixtures() {
        let datum = physical(&case);
        let memory = MemoryBudget::new(256 * 1024);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&memory, &original, &invoking);
        let output = crate::expr::datums::read_with_control(&datum, &control).unwrap();
        assert_eq!(memory.used(), output.reserved_bytes(), "{}", case["id"]);
        assert!(memory.used() > 0);
        drop(output);
        assert_eq!(memory.used(), 0);
        let small = MemoryBudget::new(0);
        let error = crate::expr::datums::read_with_control(
            &datum,
            &ProductionControl::new(&small, &original, &invoking),
        )
        .unwrap_err();
        assert_eq!(error.sqlstate(), Some("53200"), "{}", case["id"]);
        assert_eq!(small.used(), 0);
        invoking.cancel();
        assert_eq!(
            crate::expr::datums::read_with_control(&datum, &control)
                .unwrap_err()
                .sqlstate(),
            Some("57014")
        );
        let invoking = CancellationToken::new();
        original.cancel();
        assert_eq!(
            crate::expr::datums::read_with_control(
                &datum,
                &ProductionControl::new(&memory, &original, &invoking)
            )
            .unwrap_err()
            .sqlstate(),
            Some("57014")
        );
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn native_range_datums_reject_truncated_payloads_flags_offsets_and_type_mismatches() {
    let control = ProductionControl::uncontrolled();
    for case in fixtures()
        .into_iter()
        .filter(|case| !case["type"].as_str().unwrap().ends_with("[]"))
    {
        let datum = physical(&case);
        let ty = column_type(datum.type_oid());
        let payload = &datum.bytes()[4..];
        for end in 0..datum.bytes().len() {
            assert!(
                crate::expr::datums::read(&DatumValue::new(
                    datum.type_oid(),
                    0,
                    datum.bytes()[..end].to_vec()
                ))
                .is_err(),
                "{} prefix {end}",
                case["id"]
            );
        }
        let mut wrong_type = payload.to_vec();
        wrong_type[..4].fill(0);
        assert_eq!(
            read(&wrong_type, &ty, None, &control)
                .unwrap_err()
                .sqlstate(),
            Some("42704")
        );
        let mut corrupt = payload.to_vec();
        match ty {
            ColumnType::Range(_) => *corrupt.last_mut().unwrap() |= 0x40,
            ColumnType::Multirange(_) => corrupt[4..8].copy_from_slice(&u32::MAX.to_le_bytes()),
            _ => unreachable!(),
        }
        assert_eq!(
            read(&corrupt, &ty, None, &control).unwrap_err().sqlstate(),
            Some("XX001")
        );
        if case["id"].as_str().unwrap().ends_with("offsets") {
            let mut corrupt = payload.to_vec();
            corrupt[8] += 1;
            assert_eq!(
                read(&corrupt, &ty, None, &control).unwrap_err().sqlstate(),
                Some("XX001")
            );
        }
    }
}

#[test]
fn native_range_physical_comparison_retains_selected_bounds_and_original_allowance() {
    let memory = MemoryBudget::new(256 * 1024);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    for case in fixtures()
        .into_iter()
        .filter(|case| !case["type"].as_str().unwrap().ends_with("[]"))
    {
        let datum = physical(&case);
        let oid = datum.type_oid();
        let physical = Value::Datum(datum);
        let admitted = Value::Str(case["output"].as_str().unwrap().into());
        let left = RangeComparisonValue::from_declared_with_control(&physical, oid, None, &control)
            .unwrap()
            .unwrap();
        let right =
            RangeComparisonValue::from_declared_with_control(&admitted, oid, None, &control)
                .unwrap()
                .unwrap();
        assert_eq!(
            left.cmp_with_control(&right, &control).unwrap(),
            std::cmp::Ordering::Equal
        );
        drop((left, right));
        assert_eq!(memory.used(), 0);
        let row = |value| {
            Value::Row(
                uqa_core::RowValue::typed(
                    vec![value],
                    vec![uqa_core::RecordFieldType {
                        collation_oid: None,
                        oid,
                        type_modifier: -1,
                    }],
                )
                .unwrap(),
            )
        };
        assert!(crate::expr::equal_typed_values_with_enum_catalog(
            &row(physical),
            &row(admitted),
            &control,
            None
        )
        .unwrap());
        assert_eq!(memory.used(), 0);
    }
    let case = fixtures()
        .into_iter()
        .find(|case| case["id"] == "nummultirange_packed_alignment")
        .unwrap();
    let datum = Value::Datum(physical(&case));
    let small = MemoryBudget::new(0);
    let limited = ProductionControl::new(&small, &token, &token);
    assert_eq!(
        RangeComparisonValue::from_declared_with_control(&datum, 4532, None, &limited)
            .err()
            .unwrap()
            .sqlstate(),
        Some("53200")
    );
    assert_eq!(small.used(), 0);
    token.cancel();
    assert_eq!(
        RangeComparisonValue::from_declared_with_control(&datum, 4532, None, &control)
            .err()
            .unwrap()
            .sqlstate(),
        Some("57014")
    );
    assert_eq!(memory.used(), 0);
}

#[test]
fn native_range_datum_producers_admit_scratch_and_preserve_postgresql_bytes() {
    for case in fixtures()
        .into_iter()
        .filter(|case| !case["type"].as_str().unwrap().ends_with("[]"))
    {
        let datum = physical(&case);
        let expected = &datum.bytes()[4..];
        let ty = column_type(datum.type_oid());
        let text = case["output"].as_str().unwrap();
        let budget = MemoryBudget::new(1 << 20);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&budget, &original, &invoking);
        let encoded = encoding::encode_with_control(text, &ty, &control).unwrap();
        assert_eq!(&**encoded, expected, "{}", case["id"]);
        assert_eq!(budget.used(), encoded.capacity());
        assert_eq!(encoded.reserved_bytes(), encoded.capacity());
        drop(encoded);
        assert_eq!(budget.used(), 0);
        for limit in [0, 4, expected.len() - 1] {
            let budget = MemoryBudget::new(limit);
            let control = ProductionControl::new(&budget, &original, &invoking);
            assert_eq!(
                encoding::encode_with_control(text, &ty, &control)
                    .unwrap_err()
                    .sqlstate(),
                Some("53200"),
                "{} at {limit}",
                case["id"]
            );
            assert_eq!(budget.used(), 0);
        }
        for token in [&original, &invoking] {
            token.cancel();
            assert_eq!(
                encoding::encode_with_control(text, &ty, &control)
                    .unwrap_err()
                    .sqlstate(),
                Some("57014")
            );
            token.reset();
            assert_eq!(budget.used(), 0);
        }
    }
}
