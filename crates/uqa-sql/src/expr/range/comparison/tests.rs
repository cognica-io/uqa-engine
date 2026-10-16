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
use uqa_core::{memory::MemoryBudget, CancellationToken};

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
