//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{ValueImageError, ValueImageReader, ValueImageResult, WINDOW};
use crate::{memory::MemoryError, Value};
use std::io::Write;

pub(super) fn copy<'a, I: ExactSizeIterator<Item = (&'a str, &'a Value)>>(
    mut reader: ValueImageReader<'a, '_, I>,
    output: &mut dyn Write,
) -> ValueImageResult<u64> {
    let mut buffer = [0; WINDOW];
    let mut written = 0_u64;
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            return Ok(written);
        }
        let mut remaining = &buffer[..count];
        while !remaining.is_empty() {
            reader.cancellation.check()?;
            match output.write(remaining) {
                Ok(0) => return Err(std::io::Error::from(std::io::ErrorKind::WriteZero).into()),
                Ok(count) if count <= remaining.len() => {
                    remaining = &remaining[count..];
                    written = written
                        .checked_add(count as u64)
                        .ok_or(MemoryError::SizeOverflow)?;
                }
                Ok(_) => return Err(ValueImageError::Invalid("writer byte count")),
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(error) => return Err(error.into()),
            }
        }
        reader.cancellation.check()?;
    }
}
