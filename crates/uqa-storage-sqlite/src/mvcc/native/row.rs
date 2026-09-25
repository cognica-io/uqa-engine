//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Lossless native row payloads retain `SQLite` storage classes; decode borrows large text and binary fields from their charged record owner.

use rusqlite::types::ValueRef;
use uqa_core::memory::BudgetedVec;
use uqa_storage::mvcc::VersionResult;
use uqa_storage::read_control::StorageReadControl;

use super::invalid;

const PREFIX: &[u8] = b"UNR\x01";

/// Upper bound for a two-BLOB row before fetching it. Saturation only caps an unrepresentable upper bound at the address-space limit; each field still has the codec's u32 length limit.
pub(crate) fn binary_pair_limit(key_bytes: usize, value_bytes: usize) -> VersionResult<usize> {
    u32::try_from(key_bytes).map_err(|_| invalid("native key exceeds the record format length"))?;
    let envelope = PREFIX.len() + 2 + 2 * (1 + 4);
    Ok(envelope
        .saturating_add(key_bytes)
        .saturating_add(value_bytes.min(u32::MAX as usize)))
}

pub fn encode_row(
    values: &[ValueRef<'_>],
    control: &StorageReadControl,
) -> VersionResult<BudgetedVec<u8>> {
    control.cancellation().check()?;
    let columns =
        u16::try_from(values.len()).map_err(|_| invalid("native row has too many columns"))?;
    let mut output = BudgetedVec::new(control.memory());
    output.extend_from_slice(PREFIX)?;
    output.extend_from_slice(&columns.to_be_bytes())?;
    for value in values {
        control.cancellation().check()?;
        match value {
            ValueRef::Null => output.push(0)?,
            ValueRef::Integer(value) => {
                output.push(1)?;
                output.extend_from_slice(&value.to_be_bytes())?;
            }
            ValueRef::Real(value) => {
                if value.is_nan() {
                    return Err(invalid("SQLite native rows cannot retain a NaN REAL"));
                }
                output.push(2)?;
                output.extend_from_slice(&value.to_bits().to_be_bytes())?;
            }
            ValueRef::Text(bytes) => {
                std::str::from_utf8(bytes).map_err(|_| invalid("native row text is not UTF-8"))?;
                output.push(3)?;
                encode_bytes(&mut output, bytes, control)?;
            }
            ValueRef::Blob(bytes) => {
                output.push(4)?;
                encode_bytes(&mut output, bytes, control)?;
            }
        }
    }
    Ok(output)
}

fn encode_bytes(
    output: &mut BudgetedVec<u8>,
    bytes: &[u8],
    control: &StorageReadControl,
) -> VersionResult<()> {
    let length = u32::try_from(bytes.len())
        .map_err(|_| invalid("native field exceeds the record format length"))?;
    output.extend_from_slice(&length.to_be_bytes())?;
    output.reserve(bytes.len())?;
    for chunk in bytes.chunks(1024) {
        control.cancellation().check()?;
        output.extend_from_slice(chunk)?;
    }
    Ok(())
}

pub fn decode_row<'a>(
    bytes: &'a [u8],
    expected_columns: usize,
    control: &StorageReadControl,
) -> VersionResult<BudgetedVec<ValueRef<'a>>> {
    control.cancellation().check()?;
    let mut input = bytes;
    if take(&mut input, PREFIX.len())? != PREFIX {
        return Err(invalid("unknown native row codec"));
    }
    let columns = usize::from(u16::from_be_bytes(take_array(&mut input)?));
    if columns != expected_columns {
        return Err(invalid("native row column count does not match its layout"));
    }
    // Each column needs at least a tag; reject impossible counts before allocating descriptors.
    if columns > input.len() {
        return Err(invalid("truncated native row"));
    }
    let mut values = BudgetedVec::new(control.memory());
    values.reserve(columns)?;
    for _ in 0..columns {
        control.cancellation().check()?;
        let tag = take(&mut input, 1)?[0];
        let value = match tag {
            0 => ValueRef::Null,
            1 => ValueRef::Integer(i64::from_be_bytes(take_array(&mut input)?)),
            2 => {
                let value = f64::from_bits(u64::from_be_bytes(take_array(&mut input)?));
                if value.is_nan() {
                    return Err(invalid("SQLite native rows cannot retain a NaN REAL"));
                }
                ValueRef::Real(value)
            }
            3 | 4 => {
                let length = u32::from_be_bytes(take_array(&mut input)?) as usize;
                let value = take(&mut input, length)?;
                if tag == 3 {
                    std::str::from_utf8(value)
                        .map_err(|_| invalid("native row text is not UTF-8"))?;
                    ValueRef::Text(value)
                } else {
                    ValueRef::Blob(value)
                }
            }
            _ => return Err(invalid("unknown native row value tag")),
        };
        values.push(value)?;
    }
    if !input.is_empty() {
        return Err(invalid("native row has trailing bytes"));
    }
    Ok(values)
}

fn take<'a>(input: &mut &'a [u8], length: usize) -> VersionResult<&'a [u8]> {
    let (head, tail) = input
        .split_at_checked(length)
        .ok_or_else(|| invalid("truncated native row"))?;
    *input = tail;
    Ok(head)
}

fn take_array<const N: usize>(input: &mut &[u8]) -> VersionResult<[u8; N]> {
    take(input, N)?
        .try_into()
        .map_err(|_| invalid("truncated native row scalar"))
}
