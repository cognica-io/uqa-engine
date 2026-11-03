//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Versioned images preserve evaluated value carriers without SQL interpretation.

use std::io::{Read, Write};

use crate::{
    memory::{Budgeted, MemoryBudget, MemoryError},
    CancellationToken, Value, ValueRetentionError,
};

mod read;
mod write;

const MAGIC: &[u8; 8] = b"UQAVAL01";
const MAX_DEPTH: usize = 128;
const WINDOW: usize = 16 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum ValueImageError {
    #[error("invalid value image: {0}")]
    Invalid(&'static str),
    #[error(transparent)]
    Memory(#[from] MemoryError),
    #[error(transparent)]
    Cancelled(#[from] crate::QueryCancelled),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub type ValueImageResult<T> = Result<T, ValueImageError>;

impl From<ValueRetentionError> for ValueImageError {
    fn from(error: ValueRetentionError) -> Self {
        match error {
            ValueRetentionError::Memory(error) => Self::Memory(error),
            ValueRetentionError::Cancelled(error) => Self::Cancelled(error),
            ValueRetentionError::Malformed { .. } => Self::Invalid("value carrier"),
        }
    }
}

/// Stream one evaluated value with a version header. Input remains borrowed; only numerical formatting scratch uses `memory`. Writes check cancellation in at most 16 KiB windows. Failed output must be discarded by its enclosing publication owner. This codec neither captures a database nor resolves SQL types or external resources.
pub fn write_value_image(
    value: &Value,
    output: &mut dyn Write,
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> ValueImageResult<u64> {
    let mut writer = write::Writer {
        output,
        memory,
        cancellation,
        written: 0,
    };
    writer.bytes(MAGIC)?;
    writer.value(value, 0)?;
    cancellation.check()?;
    Ok(writer.written)
}

/// Write the same map image from an owner's borrowed ordered fields, without constructing another field map. Keys must be strictly increasing and the iterator must report its exact length.
pub fn write_fields_image<'a>(
    fields: impl ExactSizeIterator<Item = (&'a str, &'a Value)>,
    output: &mut dyn Write,
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> ValueImageResult<u64> {
    let mut writer = write::Writer {
        output,
        memory,
        cancellation,
        written: 0,
    };
    writer.bytes(MAGIC)?;
    writer.bytes(&[17])?;
    writer.fields_payload(fields, 0)?;
    cancellation.check()?;
    Ok(writer.written)
}

/// Read exactly one versioned value, leaving subsequent stream bytes to the enclosing format. Every decoded allocation is admitted before construction and remains charged with the returned owner. The codec bounds nesting to 128 containers. Source storage and native B-tree allocator slack retain their existing ownership boundaries.
pub fn read_value_image(
    input: &mut dyn Read,
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> ValueImageResult<Budgeted<Value>> {
    let mut reader = read::Reader {
        input,
        memory,
        cancellation,
    };
    if reader.fixed::<8>()? != *MAGIC {
        return Err(ValueImageError::Invalid("version header"));
    }
    let value = reader.value(0)?;
    cancellation.check()?;
    Ok(value)
}

/// Decode a complete standalone image, rejecting trailing bytes.
pub fn decode_value_image(
    mut input: &[u8],
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> ValueImageResult<Budgeted<Value>> {
    let value = read_value_image(&mut input, memory, cancellation)?;
    if !input.is_empty() {
        return Err(ValueImageError::Invalid("trailing bytes"));
    }
    Ok(value)
}

fn check_depth(depth: usize) -> ValueImageResult<()> {
    if depth > MAX_DEPTH {
        Err(ValueImageError::Invalid("nesting depth"))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests;
