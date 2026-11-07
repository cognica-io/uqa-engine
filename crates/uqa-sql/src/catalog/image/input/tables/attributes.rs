//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Dropped slots are reconstructed without consulting types that may no longer exist.

use super::{payload, read_list, Input};
use crate::{catalog::relation_attributes::DroppedAttribute, SQLError};
use std::io::Read;
use uqa_core::{
    memory::{Budgeted, MemoryBudget},
    CancellationToken,
};

/// Reconstruct exact dropped physical attributes; the enclosing relation validates the complete live/dropped slot layout.
pub fn read_dropped_attributes_image(
    reader: &mut dyn Read,
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<Budgeted<Vec<DroppedAttribute>>, SQLError> {
    read_list(
        reader,
        memory,
        cancellation,
        *b"UQADAT01",
        |input: &mut Input<'_>, memory| {
            Ok(DroppedAttribute {
                number: i16::from_le_bytes(input.binary.array()?),
                type_length: i64::from_le_bytes(input.binary.array()?),
                type_modifier: i64::from_le_bytes(input.binary.array()?),
                dimensions: i64::from_le_bytes(input.binary.array()?),
                by_value: input.flag()?,
                alignment: payload(input.binary.text()?, memory),
                storage: payload(input.binary.text()?, memory),
                collation: i64::from_le_bytes(input.binary.array()?),
                identity: payload(input.binary.text()?, memory),
                is_local: input.flag()?,
                inheritance_count: i64::from_le_bytes(input.binary.array()?),
            })
        },
    )
}
