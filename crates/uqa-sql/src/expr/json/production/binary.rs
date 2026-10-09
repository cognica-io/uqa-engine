//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` JSONB container bytes, independent of the current composite descriptor.

use crate::error::{Result, SQLError};
use uqa_core::memory::{Produced, ProductionControl};

mod compare;
mod decode;
mod encode;
mod walk;
pub(in crate::expr) use compare::{compare_jsonb_datums_with_control, JsonbInput};
#[cfg(test)]
mod tests;

const COUNT_MASK: u32 = 0x0fff_ffff;
const SCALAR: u32 = 0x1000_0000;
const OBJECT: u32 = 0x2000_0000;
const ARRAY: u32 = 0x4000_0000;
const NUMERIC: u32 = 0x1000_0000;
const FALSE: u32 = 0x2000_0000;
const TRUE: u32 = 0x3000_0000;
const NULL: u32 = 0x4000_0000;
const CONTAINER: u32 = 0x5000_0000;
const TYPE_MASK: u32 = 0x7000_0000;
const HAS_OFFSET: u32 = 0x8000_0000;

pub(in crate::expr) fn encode_jsonb_datum(text: &str) -> Result<Vec<u8>> {
    encode::encode(text)
}

pub(in crate::expr) fn decode_jsonb_datum_with_control(
    bytes: &[u8],
    control: &ProductionControl<'_>,
) -> Result<Produced<String>> {
    decode::decode(bytes, control)
}

fn corrupt() -> SQLError {
    SQLError::Routine {
        sqlstate: "XX001".into(),
        message: "invalid jsonb datum length".into(),
    }
}

fn internal(message: &str) -> SQLError {
    SQLError::Routine {
        sqlstate: "XX000".into(),
        message: message.into(),
    }
}

fn aligned(offset: usize) -> Result<usize> {
    Ok(offset.checked_add(3).ok_or_else(corrupt)? & !3)
}

fn word(bytes: &[u8], offset: usize) -> Result<u32> {
    let end = offset.checked_add(4).ok_or_else(corrupt)?;
    let bytes = bytes.get(offset..end).ok_or_else(corrupt)?;
    Ok(u32::from_le_bytes(bytes.try_into().expect("four bytes")))
}
