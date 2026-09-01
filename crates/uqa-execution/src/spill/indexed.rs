//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};

use tempfile::NamedTempFile;

use crate::batch::{PhysicalRow, RowSchema};
use crate::physical::ExecResult;

use super::format::{
    decode_physical_row_record, encode_physical_row_record, encoded_physical_row_record_size,
    spill_error, RECORD_PREFIX_BYTES,
};

#[cfg(test)]
mod tests;

/// Disk-only physical-row store with constant-memory positional lookup.
///
/// Each row retains the exact physical layout described by `schema` and is
/// encoded with the same positional tagged representation as [`super::SpillBuffer`].
/// Record offsets live in a second temporary file, so even a partition with
/// billions of rows does not create an in-memory offset table. A single decoded
/// physical row is the only input-sized allocation retained by [`Self::get`].
/// Both files are unlinked by `NamedTempFile` on drop.
pub struct IndexedSpill {
    schema: RowSchema,
    data: NamedTempFile,
    offsets: NamedTempFile,
    rows: u64,
    encoded_bytes: u64,
}

impl IndexedSpill {
    pub fn new(input_schema: RowSchema) -> ExecResult<Self> {
        Ok(Self {
            schema: input_schema,
            data: NamedTempFile::new().map_err(|error| {
                spill_error(format!("failed to create indexed spill data: {error}"))
            })?,
            offsets: NamedTempFile::new().map_err(|error| {
                spill_error(format!("failed to create indexed spill offsets: {error}"))
            })?,
            rows: 0,
            encoded_bytes: 0,
        })
    }

    pub fn len(&self) -> u64 {
        self.rows
    }

    pub fn is_empty(&self) -> bool {
        self.rows == 0
    }

    pub fn encoded_bytes(&self) -> u64 {
        self.encoded_bytes
    }

    pub fn row_schema(&self) -> &RowSchema {
        &self.schema
    }

    pub(crate) fn encoded_row_size(schema: &RowSchema, row: &PhysicalRow) -> ExecResult<usize> {
        encoded_physical_row_record_size(row, schema.physical_width())?
            .checked_add(RECORD_PREFIX_BYTES)
            .ok_or_else(|| spill_error("indexed spill row size overflow"))
    }

    /// Append one indivisible row. Failed writes roll both files back to their
    /// original lengths, so callers never observe a partial index entry.
    pub fn push(&mut self, row: &PhysicalRow) -> ExecResult<()> {
        let payload = encode_physical_row_record(row, self.schema.physical_width())?;
        let length = u64::try_from(payload.len())
            .map_err(|_| spill_error("indexed spill row is too large"))?;
        // Validate every piece of metadata before touching either file.  A
        // counter overflow after the append would otherwise return an error
        // while leaving a physically visible row whose offset/count was not
        // published consistently.
        let next_rows = self
            .rows
            .checked_add(1)
            .ok_or_else(|| spill_error("indexed spill row count overflow"))?;
        let record_bytes = length
            .checked_add(8)
            .ok_or_else(|| spill_error("indexed spill row length overflow"))?;
        let next_encoded_bytes = self
            .encoded_bytes
            .checked_add(record_bytes)
            .ok_or_else(|| spill_error("indexed spill byte count overflow"))?;
        let data_length = self
            .data
            .as_file_mut()
            .seek(SeekFrom::End(0))
            .map_err(|error| spill_error(format!("failed to seek indexed spill data: {error}")))?;
        let offsets_length =
            self.offsets
                .as_file_mut()
                .seek(SeekFrom::End(0))
                .map_err(|error| {
                    spill_error(format!("failed to seek indexed spill offsets: {error}"))
                })?;

        let write_result = (|| -> std::io::Result<()> {
            self.data.as_file_mut().write_all(&length.to_le_bytes())?;
            self.data.as_file_mut().write_all(&payload)?;
            self.data.as_file_mut().flush()?;
            self.offsets
                .as_file_mut()
                .write_all(&data_length.to_le_bytes())?;
            self.offsets.as_file_mut().flush()
        })();
        if let Err(error) = write_result {
            let data_rollback = self.data.as_file_mut().set_len(data_length);
            let offsets_rollback = self.offsets.as_file_mut().set_len(offsets_length);
            let rollback_error = match (data_rollback, offsets_rollback) {
                (Ok(()), Ok(())) => None,
                (Err(data), Ok(())) => Some(format!("data rollback failed: {data}")),
                (Ok(()), Err(offsets)) => Some(format!("offset rollback failed: {offsets}")),
                (Err(data), Err(offsets)) => Some(format!(
                    "data rollback failed: {data}; offset rollback failed: {offsets}"
                )),
            };
            if let Some(rollback) = rollback_error {
                return Err(spill_error(format!(
                    "failed to append indexed spill row: {error}; {rollback}"
                )));
            }
            return Err(spill_error(format!(
                "failed to append indexed spill row: {error}"
            )));
        }

        self.rows = next_rows;
        self.encoded_bytes = next_encoded_bytes;
        Ok(())
    }

    /// Decode the row at `index` without loading any other row or index entry.
    pub fn get(&mut self, index: u64) -> ExecResult<PhysicalRow> {
        if index >= self.rows {
            return Err(spill_error(format!(
                "indexed spill row {index} is outside 0..{}",
                self.rows
            )));
        }
        let expected_offsets_length = self
            .rows
            .checked_mul(8)
            .ok_or_else(|| spill_error("indexed spill offsets length overflow"))?;
        let actual_offsets_length = self
            .offsets
            .as_file()
            .metadata()
            .map_err(|error| {
                spill_error(format!("failed to inspect indexed spill offsets: {error}"))
            })?
            .len();
        if actual_offsets_length != expected_offsets_length {
            return Err(spill_error(format!(
                "indexed spill offsets length {actual_offsets_length} does not match expected {expected_offsets_length}"
            )));
        }
        let data_length = self
            .data
            .as_file()
            .metadata()
            .map_err(|error| spill_error(format!("failed to inspect indexed spill data: {error}")))?
            .len();
        let offset_position = index
            .checked_mul(8)
            .ok_or_else(|| spill_error("indexed spill offset overflow"))?;
        let offset = read_indexed_offset(self.offsets.as_file_mut(), offset_position)?;
        let record_end = if index
            .checked_add(1)
            .ok_or_else(|| spill_error("indexed spill row index overflow"))?
            < self.rows
        {
            read_indexed_offset(
                self.offsets.as_file_mut(),
                offset_position
                    .checked_add(8)
                    .ok_or_else(|| spill_error("indexed spill next offset overflow"))?,
            )?
        } else {
            data_length
        };
        let payload_start = offset
            .checked_add(8)
            .ok_or_else(|| spill_error("indexed spill payload offset overflow"))?;
        if payload_start > record_end || record_end > data_length {
            return Err(spill_error(format!(
                "indexed spill record bounds {offset}..{record_end} are outside data length {data_length}"
            )));
        }
        self.data
            .as_file_mut()
            .seek(SeekFrom::Start(offset))
            .map_err(|error| spill_error(format!("failed to seek indexed spill row: {error}")))?;
        let mut length = [0_u8; 8];
        self.data
            .as_file_mut()
            .read_exact(&mut length)
            .map_err(|error| {
                spill_error(format!("failed to read indexed spill length: {error}"))
            })?;
        let declared_length = u64::from_le_bytes(length);
        let available_length = record_end - payload_start;
        if declared_length != available_length {
            return Err(spill_error(format!(
                "indexed spill row length {declared_length} does not match record payload {available_length}"
            )));
        }
        let length = usize::try_from(declared_length)
            .map_err(|_| spill_error("indexed spill row length is outside address space"))?;
        let mut payload = Vec::new();
        payload.try_reserve_exact(length).map_err(|error| {
            spill_error(format!(
                "unable to allocate indexed spill row payload of {length} bytes: {error}"
            ))
        })?;
        payload.resize(length, 0);
        self.data
            .as_file_mut()
            .read_exact(&mut payload)
            .map_err(|error| spill_error(format!("failed to read indexed spill row: {error}")))?;
        decode_physical_row_record(&payload, self.schema.physical_width())
    }
}

fn read_indexed_offset(file: &mut File, position: u64) -> ExecResult<u64> {
    file.seek(SeekFrom::Start(position))
        .map_err(|error| spill_error(format!("failed to seek indexed spill offset: {error}")))?;
    let mut encoded = [0_u8; 8];
    file.read_exact(&mut encoded)
        .map_err(|error| spill_error(format!("failed to read indexed spill offset: {error}")))?;
    Ok(u64::from_le_bytes(encoded))
}
