//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` varlena compression headers select PGLZ or LZ4 independently of the current default compression setting.

use crate::SQLError;
use uqa_core::memory::{Produced, ProductionControl, ProductionVec};

pub(super) fn corrupt(method: u32) -> SQLError {
    match method {
        0 | 1 => SQLError::Routine {
            sqlstate: "XX001".into(),
            message: format!(
                "compressed {} data is corrupt",
                if method == 0 { "pglz" } else { "lz4" }
            ),
        },
        _ => SQLError::Routine {
            sqlstate: "XX000".into(),
            message: format!("invalid compression method id {method}"),
        },
    }
}

pub(super) fn decompress(
    input: &[u8],
    length: usize,
    method: u32,
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<u8>>, SQLError> {
    if method > 1 || (input.is_empty() && length != 0) {
        return Err(corrupt(method));
    }
    let mut output = ProductionVec::new(*control);
    output.reserve(length)?;
    for _ in 0..length {
        output.push_copy(0_u8)?;
    }
    let (mut output, memory) = output.finish()?.into_parts();
    let valid = if method == 0 {
        pglz(input, &mut output, control)?
    } else {
        control.check()?;
        lz4_flex::block::decompress_into(input, &mut output).is_ok_and(|written| written == length)
    };
    control.check()?;
    if !valid {
        return Err(corrupt(method));
    }
    Ok(control.finish(output, memory)?)
}

fn pglz(
    input: &[u8],
    output: &mut [u8],
    control: &ProductionControl<'_>,
) -> Result<bool, SQLError> {
    let (mut source, mut destination) = (0, 0);
    while source < input.len() && destination < output.len() {
        control.check()?;
        let flags = input[source];
        source += 1;
        for bit in 0..8 {
            if source == input.len() || destination == output.len() {
                break;
            }
            if flags & (1 << bit) == 0 {
                output[destination] = input[source];
                source += 1;
                destination += 1;
                continue;
            }
            let Some(pair) = input.get(source..source + 2) else {
                return Ok(false);
            };
            source += 2;
            let distance = (usize::from(pair[0] & 0xf0) << 4) | usize::from(pair[1]);
            let mut length = usize::from(pair[0] & 0x0f) + 3;
            if length == 18 {
                let Some(extension) = input.get(source) else {
                    return Ok(false);
                };
                source += 1;
                length += usize::from(*extension);
            }
            if distance == 0 || distance > destination {
                return Ok(false);
            }
            for _ in 0..length.min(output.len() - destination) {
                output[destination] = output[destination - distance];
                destination += 1;
            }
        }
    }
    Ok(source == input.len() && destination == output.len())
}
