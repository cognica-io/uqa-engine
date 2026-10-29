//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::super::*;
use crate::ast::EnumTypeReference;
use uqa_core::{memory::MemoryBudget, CancellationToken};

#[test]
fn canonical_catalog_identity_is_admitted_before_its_first_destination() {
    let ty = ColumnType::Array(Box::new(ColumnType::Array(Box::new(ColumnType::Enum(
        EnumTypeReference {
            schema: "public".into(),
            name: "item".into(),
            oid: 20_001,
            array_oid: 20_002,
        },
    )))));
    let cancellation = CancellationToken::new();
    let budget = MemoryBudget::new(0);
    let control = ProductionControl::new(&budget, &cancellation, &cancellation);
    let measured = allocation_counter::measure(|| {
        assert!(matches!(
            canonical_column_type_name_with_control(&ty, &control),
            Err(ValueRetentionError::Memory(_))
        ));
    });
    assert_eq!(measured.count_total, 0);
    let budget = MemoryBudget::new(4096);
    let control = ProductionControl::new(&budget, &cancellation, &cancellation);
    let mut output = None;
    let measured = allocation_counter::measure(|| {
        output = Some(canonical_column_type_name_with_control(&ty, &control).unwrap());
    });
    let output = output.unwrap();
    assert_eq!(output.as_str(), "enum#20001[]");
    assert_eq!(measured.bytes_current, budget.used() as i64);
    assert_eq!(budget.used(), output.capacity());
    drop(output);
    assert_eq!(budget.used(), 0);
}
