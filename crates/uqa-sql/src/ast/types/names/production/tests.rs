//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::ast::{CompositeTypeReference, EnumTypeReference};
use uqa_core::{memory::MemoryBudget, CancellationToken};

#[test]
fn dynamic_type_names_reject_before_quoting_unadmitted_payloads() {
    let name = "quoted\"type".repeat(1 << 15);
    let types = [
        ColumnType::Enum(EnumTypeReference {
            schema: "hidden".into(),
            name: name.clone(),
            oid: 20_001,
            array_oid: 20_002,
        }),
        ColumnType::Composite(CompositeTypeReference {
            schema: "hidden".into(),
            name: name.clone(),
            oid: 20_003,
            array_oid: 20_004,
            relation_oid: 20_005,
        }),
        ColumnType::Domain {
            schema: "hidden".into(),
            name,
            oid: 20_006,
            array_oid: Some(20_007),
            collation_oid: None,
            base: Box::new(ColumnType::Text),
        },
    ];
    for ty in types {
        for regtype in [false, true] {
            let budget = MemoryBudget::new(0);
            let cancellation = CancellationToken::new();
            let control = ProductionControl::new(&budget, &cancellation, &cancellation);
            let measured = allocation_counter::measure(|| {
                assert!(matches!(
                    super::name(&ty, regtype, &control),
                    Err(ValueRetentionError::Memory(_))
                ));
            });
            assert_eq!(measured.count_total, 0);
            assert_eq!(budget.used(), 0);
        }
    }
}

#[test]
fn dynamic_array_type_names_keep_visibility_quoting_and_only_the_final_owner() {
    let ty = ColumnType::Array(Box::new(ColumnType::Array(Box::new(ColumnType::Enum(
        EnumTypeReference {
            schema: "A.Schema".into(),
            name: "quoted\"type".into(),
            oid: 20_001,
            array_oid: 20_002,
        },
    )))));
    for regtype in [false, true] {
        let expected = if regtype {
            ty.regtype_name()
        } else {
            ty.sql_name()
        };
        let budget = MemoryBudget::new(4096);
        let cancellation = CancellationToken::new();
        let control = ProductionControl::new(&budget, &cancellation, &cancellation);
        let mut output = None;
        let measured = allocation_counter::measure(|| {
            output = Some(super::name(&ty, regtype, &control).unwrap());
        });
        let output = output.unwrap();
        assert_eq!(output.as_str(), expected);
        assert_eq!(measured.bytes_current, budget.used() as i64);
        assert_eq!(budget.used(), output.capacity());
        drop(output);
        assert_eq!(budget.used(), 0);
    }
}
