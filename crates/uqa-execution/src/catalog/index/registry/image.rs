//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native secondary-index metadata retains its original opaque encodings and durable membership.

use super::{CapturedCatalogIndexes, CatalogIndexes};
use std::{cmp::Ordering, io::Read, ops::Bound};
use uqa_core::{
    image_io::{decode_image, AsyncImageInput, ImageDecoder},
    memory::{Budgeted, MemoryBudget, MemoryReservation},
    CancellationToken, RelationIdentity,
};
use uqa_sql::SQLError;
use uqa_storage::{
    image_io::{ImageRead, ImageReader},
    read_control::StorageReadControl,
    CatalogIndexRow, StorageBackendError, StorageBackendResult,
};

mod output;

const MAGIC: [u8; 8] = *b"UQAIDX01";

fn invalid(message: &'static str) -> SQLError {
    SQLError::Routine {
        sqlstate: "XX001".into(),
        message: format!("invalid index catalog image: {message}"),
    }
}

async fn text(
    input: &mut AsyncImageInput<'_>,
    memory: &mut MemoryReservation,
    identifier: bool,
) -> Result<String, SQLError> {
    let value = input.text().await?;
    if identifier {
        if value.is_empty() {
            return Err(invalid("empty identifier"));
        }
        for chunk in value.as_bytes().chunks(4096) {
            input.checkpoint().await?;
            if chunk.contains(&0) {
                return Err(invalid("identifier contains NUL"));
            }
        }
    }
    let (value, lease) = value.into_parts();
    memory.absorb(lease);
    Ok(value)
}

async fn compare(
    left: &str,
    right: &str,
    input: &mut AsyncImageInput<'_>,
) -> Result<Ordering, SQLError> {
    for (left, right) in left
        .as_bytes()
        .chunks(4096)
        .zip(right.as_bytes().chunks(4096))
    {
        input.checkpoint().await?;
        let order = left.cmp(right);
        if order != Ordering::Equal {
            return Ok(order);
        }
    }
    input.cancellation.check()?;
    Ok(left.len().cmp(&right.len()))
}

impl CatalogIndexes {
    /// Reconstruct an unpublished durable registry, transferring admitted field buffers into their native rows. Complete restore validates referenced tables and the opaque index definitions before activation.
    pub fn read_image(
        reader: &mut dyn Read,
        memory: &MemoryBudget,
        cancellation: &CancellationToken,
    ) -> Result<Self, SQLError> {
        Self::read_image_with_mutation_budget(reader, memory, memory, cancellation)
    }

    /// Decode original entries under `memory` and fund later native edits from `mutations`. Original payloads and retained predecessors keep their input reservations independently of future mutations and input cancellation.
    pub fn read_image_with_mutation_budget(
        reader: &mut dyn Read,
        memory: &MemoryBudget,
        mutations: &MemoryBudget,
        cancellation: &CancellationToken,
    ) -> Result<Self, SQLError> {
        decode_image(reader, memory, cancellation, async |input| {
            Self::decode(input, mutations).await
        })
    }

    /// Retain exact native fields, ordered identities and independent key copies across byte and poll windows.
    pub fn image_input(
        memory: &MemoryBudget,
        mutations: &MemoryBudget,
        cancellation: &CancellationToken,
    ) -> Result<ImageDecoder<Self, SQLError>, SQLError> {
        let mutations = mutations.clone();
        Ok(ImageDecoder::new(
            memory,
            cancellation,
            |mut reader| async move { Self::decode(reader.input(), &mutations).await },
        )?)
    }

    async fn decode(
        mut input: AsyncImageInput<'_>,
        mutations: &MemoryBudget,
    ) -> Result<Self, SQLError> {
        if input.array::<8>().await? != MAGIC {
            return Err(invalid("version header"));
        }
        let count = input.count().await?;
        let mut result = Self::with_budget(input.memory);
        for _ in 0..count {
            input.checkpoint().await?;
            let mut lease = input.memory.empty_reservation();
            let relation = RelationIdentity {
                schema: text(&mut input, &mut lease, true).await?,
                name: text(&mut input, &mut lease, true).await?,
            };
            if let Some((previous, _)) = result.durable.last_before(Bound::Unbounded) {
                let mut order = compare(&previous.schema, &relation.schema, &mut input).await?;
                if order == Ordering::Equal {
                    order = compare(&previous.name, &relation.name, &mut input).await?;
                }
                if order != Ordering::Less {
                    return Err(invalid("record order"));
                }
            }
            let mut row = CatalogIndexRow {
                relation,
                index_type: text(&mut input, &mut lease, true).await?,
                table_name: text(&mut input, &mut lease, true).await?,
                columns_json: text(&mut input, &mut lease, false).await?,
                parameters_json: text(&mut input, &mut lease, false).await?,
                definition_json: None,
            };
            row.definition_json = match input.array::<1>().await?[0] {
                0 => None,
                1 => Some(text(&mut input, &mut lease, false).await?),
                _ => return Err(invalid("definition presence tag")),
            };
            let (schema, schema_memory) = input.copy_text(&row.relation.schema).await?.into_parts();
            lease.absorb(schema_memory);
            let (name, name_memory) = input.copy_text(&row.relation.name).await?.into_parts();
            lease.absorb(name_memory);
            let row = Budgeted::new(row, lease);
            let bytes = row.reserved_bytes();
            result
                .durable
                .insert(RelationIdentity { schema, name }, row, bytes)?;
        }
        input.end().await?;
        result.durable = result.durable.into_mutation_budget(mutations);
        result.temporary = result.temporary.into_mutation_budget(mutations);
        Ok(result)
    }
}

#[cfg(test)]
mod tests;
