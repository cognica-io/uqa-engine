//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Length-delimited sort keys and arguments, using the physical spill representation for SQL values.

use super::{AggregateSortKey, OrderedRecord};
use crate::physical::physical_exec_error;
use crate::spill::{
    add_value_size, encode_value, read_bounded_spill_record, write_raw, write_u64, BinaryReader,
};
use crate::{ExecError, ExecResult};
use std::io::{BufRead, Write};
use uqa_sql::SQLError;

const DESCRIPTION: &str = "aggregate spill row";

pub(super) fn write<R: OrderedRecord>(
    writer: &mut impl Write,
    record: &R,
) -> Result<usize, SQLError> {
    encode(writer, record).map_err(physical_exec_error)
}

fn encode<R: OrderedRecord>(writer: &mut impl Write, record: &R) -> ExecResult<usize> {
    let keys = record.keys();
    let arguments = record.inputs();
    let overflow = || ExecError::Other("aggregate spill row length overflow".into());
    // Sequence, sort-key count, argument count and one flags byte per key.
    let mut payload_bytes = (3 * size_of::<u64>())
        .checked_add(keys.len())
        .ok_or_else(overflow)?;
    for value in keys.iter().map(|key| &key.value).chain(arguments) {
        add_value_size(&mut payload_bytes, value, 0)?;
    }
    let record_bytes = payload_bytes
        .checked_add(size_of::<u64>())
        .ok_or_else(overflow)?;
    write_u64(writer, payload_bytes)?;
    write_raw(
        writer,
        &record.sequence().to_le_bytes(),
        "aggregate sequence number",
    )?;
    write_u64(writer, keys.len())?;
    for key in keys {
        let flags = u8::from(key.descending) | (u8::from(key.nulls_first) << 1);
        write_raw(writer, &[flags], "aggregate sort key flags")?;
        encode_value(writer, &key.value, 0)?;
    }
    write_u64(writer, arguments.len())?;
    for value in arguments {
        encode_value(writer, value, 0)?;
    }
    Ok(record_bytes)
}

pub(super) fn read<R: OrderedRecord>(
    reader: &mut impl BufRead,
    max_record_bytes: usize,
) -> Result<Option<R>, SQLError> {
    let bytes = read_bounded_spill_record(reader, max_record_bytes, DESCRIPTION)
        .map_err(physical_exec_error)?;
    bytes
        .as_deref()
        .map(decode)
        .transpose()
        .map_err(physical_exec_error)
}

fn decode<R: OrderedRecord>(payload: &[u8]) -> ExecResult<R> {
    let mut reader = BinaryReader::new(payload);
    let sequence = reader.read_u64("aggregate sequence number")?;
    let key_count = reader.read_count("aggregate sort key count", 2)?;
    let mut keys = Vec::new();
    keys.try_reserve_exact(key_count).map_err(|err| {
        ExecError::Other(format!(
            "unable to allocate {key_count} aggregate sort keys: {err}"
        ))
    })?;
    for _ in 0..key_count {
        let flags = reader.read_u8("aggregate sort key flags")?;
        if flags & !3 != 0 {
            return Err(ExecError::Other(format!(
                "invalid aggregate sort key flags {flags}"
            )));
        }
        keys.push(AggregateSortKey {
            value: reader.read_value(0)?,
            descending: flags & 1 != 0,
            nulls_first: flags & 2 != 0,
        });
    }
    let count = reader.read_count("aggregate argument count", 1)?;
    let input = R::read_input(&mut reader, count)?;
    reader.finish(DESCRIPTION)?;
    Ok(R::new(input, keys, sequence))
}
