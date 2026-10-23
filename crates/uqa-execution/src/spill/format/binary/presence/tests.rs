//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_sql::ast::{ColumnType, InternalRelationId};

fn binding(relation: u64, attribute: u64) -> Vec<u8> {
    let mut bytes = 1_u64.to_le_bytes().to_vec();
    bytes.extend_from_slice(b"r");
    bytes.extend_from_slice(&relation.to_le_bytes());
    bytes.extend_from_slice(&attribute.to_le_bytes());
    bytes
}

#[test]
fn malformed_presence_records_fail_before_exposing_a_schema() {
    let marker = InternalRelationId::allocate().column(0);
    let schema = RowSchema::append_internal_typed(
        &RowSchema::new(vec!["r.k".into()]),
        &[(marker, Some(ColumnType::Boolean))],
    );
    let valid = binding(marker.relation().raw(), 0);
    let mut duplicate = 2_u64.to_le_bytes().to_vec();
    duplicate.extend_from_slice(&valid);
    duplicate.extend_from_slice(&valid);
    let mut wrong_marker = 1_u64.to_le_bytes().to_vec();
    wrong_marker.extend_from_slice(&binding(marker.relation().raw(), 1));
    let mut oversized_attribute = 1_u64.to_le_bytes().to_vec();
    oversized_attribute.extend_from_slice(&binding(marker.relation().raw(), u64::MAX));
    for (bytes, expected) in [
        (duplicate, "duplicate whole-row presence qualifier"),
        (wrong_marker, "requires a Boolean internal attribute"),
        (oversized_attribute, "attribute exceeds u32"),
        (u64::MAX.to_le_bytes().to_vec(), "whole-row presence count"),
    ] {
        let error = decode(&mut BinaryReader::new(&bytes), schema.clone()).unwrap_err();
        assert!(error.to_string().contains(expected), "{error}");
    }
}
