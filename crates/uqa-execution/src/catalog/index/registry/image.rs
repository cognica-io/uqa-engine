//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native secondary-index metadata retains its original opaque encodings and durable membership.

use super::{CapturedCatalogIndexes, CatalogIndexes};
use std::{cmp::Ordering, io::Read, ops::Bound};
use uqa_core::{
    image_io::ImageInput,
    memory::{Budgeted, MemoryBudget, MemoryReservation, ProductionControl},
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

fn text(
    input: &mut ImageInput<'_>,
    memory: &mut MemoryReservation,
    identifier: bool,
) -> Result<String, SQLError> {
    let value = input.text()?;
    if identifier {
        if value.is_empty() {
            return Err(invalid("empty identifier"));
        }
        for chunk in value.as_bytes().chunks(4096) {
            input.cancellation.check()?;
            if chunk.contains(&0) {
                return Err(invalid("identifier contains NUL"));
            }
        }
    }
    let (value, lease) = value.into_parts();
    memory.absorb(lease);
    Ok(value)
}

fn compare(
    left: &str,
    right: &str,
    cancellation: &CancellationToken,
) -> Result<Ordering, SQLError> {
    for (left, right) in left
        .as_bytes()
        .chunks(4096)
        .zip(right.as_bytes().chunks(4096))
    {
        cancellation.check()?;
        let order = left.cmp(right);
        if order != Ordering::Equal {
            return Ok(order);
        }
    }
    cancellation.check()?;
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
        let mut input = ImageInput {
            reader,
            memory,
            cancellation,
        };
        if input.array::<8>()? != MAGIC {
            return Err(invalid("version header"));
        }
        let count = input.count()?;
        let mut result = Self::with_budget(memory);
        let control = ProductionControl::new(memory, cancellation, cancellation);
        for _ in 0..count {
            cancellation.check()?;
            let mut lease = memory.empty_reservation();
            let relation = RelationIdentity {
                schema: text(&mut input, &mut lease, true)?,
                name: text(&mut input, &mut lease, true)?,
            };
            if let Some((previous, _)) = result.durable.last_before(Bound::Unbounded) {
                let mut order = compare(&previous.schema, &relation.schema, cancellation)?;
                if order == Ordering::Equal {
                    order = compare(&previous.name, &relation.name, cancellation)?;
                }
                if order != Ordering::Less {
                    return Err(invalid("record order"));
                }
            }
            let mut row = CatalogIndexRow {
                relation,
                index_type: text(&mut input, &mut lease, true)?,
                table_name: text(&mut input, &mut lease, true)?,
                columns_json: text(&mut input, &mut lease, false)?,
                parameters_json: text(&mut input, &mut lease, false)?,
                definition_json: None,
            };
            row.definition_json = match input.array::<1>()?[0] {
                0 => None,
                1 => Some(text(&mut input, &mut lease, false)?),
                _ => return Err(invalid("definition presence tag")),
            };
            let (schema, schema_memory) = control.copy_text(&row.relation.schema)?.into_parts();
            let (name, name_memory) = control.copy_text(&row.relation.name)?.into_parts();
            lease.absorb(schema_memory.expect("admitted index key"));
            lease.absorb(name_memory.expect("admitted index key"));
            let row = Budgeted::new(row, lease);
            let bytes = row.reserved_bytes();
            result
                .durable
                .insert(RelationIdentity { schema, name }, row, bytes)?;
        }
        input.end()?;
        result.durable = result.durable.into_mutation_budget(mutations);
        result.temporary = result.temporary.into_mutation_budget(mutations);
        Ok(result)
    }
}

#[cfg(test)]
mod tests;
