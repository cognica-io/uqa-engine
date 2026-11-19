//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Dropped slots are reconstructed without consulting types that may no longer exist.

use super::{payload, read_list, read_list_from, AsyncImageInput, Fields, ImageValue};
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
    read_list(reader, memory, cancellation, *b"UQADAT01")
}

/// Resume exact physical dropped slots on the enclosing image input.
pub async fn read_dropped_attributes_image_from(
    input: AsyncImageInput<'_>,
) -> Result<Budgeted<Vec<DroppedAttribute>>, SQLError> {
    Ok(read_list_from(input, *b"UQADAT01").await?.0)
}

impl ImageValue for DroppedAttribute {
    async fn read(input: &mut Fields<'_>) -> Result<Budgeted<Self>, SQLError> {
        let mut memory = input.binary.memory.empty_reservation();
        let value = DroppedAttribute {
            number: i16::from_le_bytes(input.binary.array().await?),
            type_length: i64::from_le_bytes(input.binary.array().await?),
            type_modifier: i64::from_le_bytes(input.binary.array().await?),
            dimensions: i64::from_le_bytes(input.binary.array().await?),
            by_value: input.flag().await?,
            alignment: payload(input.binary.text().await?, &mut memory),
            storage: payload(input.binary.text().await?, &mut memory),
            collation: i64::from_le_bytes(input.binary.array().await?),
            identity: payload(input.binary.text().await?, &mut memory),
            is_local: input.flag().await?,
            inheritance_count: i64::from_le_bytes(input.binary.array().await?),
        };
        Ok(Budgeted::new(value, memory))
    }
}
