//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Spill record framing shared by the batch and indexed-row codecs.

use std::fs::File;
use std::io::{BufReader, Read};

use tempfile::NamedTempFile;

use crate::physical::{ExecError, ExecResult};

use super::SPILL_MAGIC;

mod binary;
mod indexed;

pub(super) use binary::{
    append_batches, decode_batch, encoded_batch_size, encoded_named_single_row_batch_size,
    encoded_single_row_batch_size,
};
pub(super) use indexed::{decode_row, ExactRow};

pub(super) const RECORD_PREFIX_BYTES: usize = std::mem::size_of::<u64>();

pub(super) fn spill_error(message: impl Into<String>) -> ExecError {
    ExecError::Other(message.into())
}

pub(super) fn read_bounded_spill_record<R: Read>(
    reader: &mut R,
    max_record_bytes: usize,
    description: &str,
) -> ExecResult<Option<Vec<u8>>> {
    let mut prefix = [0u8; RECORD_PREFIX_BYTES];
    match reader.read(&mut prefix[..1]) {
        Ok(0) => return Ok(None),
        Ok(1) => {}
        Ok(_) => unreachable!(),
        Err(error) => {
            return Err(spill_error(format!(
                "failed to read {description} length: {error}"
            )))
        }
    }
    reader
        .read_exact(&mut prefix[1..])
        .map_err(|error| spill_error(format!("truncated {description} length prefix: {error}")))?;
    let payload_bytes = usize::try_from(u64::from_le_bytes(prefix))
        .map_err(|_| spill_error(format!("{description} length exceeds address space")))?;
    let record_bytes = payload_bytes
        .checked_add(RECORD_PREFIX_BYTES)
        .ok_or_else(|| spill_error(format!("{description} length overflow")))?;
    if record_bytes > max_record_bytes {
        return Err(spill_error(format!(
            "{description} exceeds recorded maximum of {max_record_bytes} bytes"
        )));
    }
    let mut record = Vec::new();
    record.try_reserve_exact(payload_bytes).map_err(|error| {
        spill_error(format!(
            "unable to allocate {payload_bytes} bytes for {description}: {error}"
        ))
    })?;
    record.resize(payload_bytes, 0);
    reader
        .read_exact(&mut record)
        .map_err(|error| spill_error(format!("truncated {description} payload: {error}")))?;
    Ok(Some(record))
}

pub(super) fn open_spill_reader(file: &NamedTempFile) -> ExecResult<BufReader<File>> {
    let reopened = file
        .reopen()
        .map_err(|error| spill_error(format!("failed to reopen spill file: {error}")))?;
    let mut reader = BufReader::new(reopened);
    let mut magic = [0u8; SPILL_MAGIC.len()];
    reader
        .read_exact(&mut magic)
        .map_err(|error| spill_error(format!("failed to read spill header: {error}")))?;
    if magic != SPILL_MAGIC {
        return Err(spill_error("invalid spill file header"));
    }
    Ok(reader)
}
