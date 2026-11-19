//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Synchronous output drains the same borrowed encoder as resumable readers.

use super::{
    stream::Encoder, ImageOutput, MemoryGraphStore, StorageBackendResult, StorageReadControl,
};

pub(super) fn write(
    store: &MemoryGraphStore,
    writer: &mut dyn std::io::Write,
    control: &StorageReadControl,
) -> StorageBackendResult<u64> {
    let mut encoder = Encoder::new(store, control);
    let mut out = ImageOutput {
        writer,
        control,
        written: 0,
    };
    let mut buffer = [0; 16 * 1024];
    loop {
        let count = encoder.read(&mut buffer)?;
        if count == 0 {
            return Ok(out.written);
        }
        out.bytes(&buffer[..count])?;
    }
}
