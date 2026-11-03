//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Original source cancellation accompanies independently controlled image output.

use super::{StorageBackendError, StorageBackendResult, StorageReadControl, Write};

/// Preserve the retained source cancellation during every output attempt and after completion, keeping its typed failure independently of codec I/O errors.
pub fn with_retained_control<T>(
    output: &mut dyn Write,
    original: Option<&StorageReadControl>,
    write: impl FnOnce(&mut dyn Write) -> StorageBackendResult<T>,
) -> StorageBackendResult<T> {
    if let Some(original) = original {
        original.check()?;
    }
    let mut output = RetainedOutput {
        output,
        original,
        error: None,
    };
    let result = write(&mut output);
    if let Some(error) = output.error {
        return Err(error);
    }
    let result = result?;
    if let Some(original) = original {
        original.check()?;
    }
    Ok(result)
}

struct RetainedOutput<'a> {
    output: &'a mut dyn Write,
    original: Option<&'a StorageReadControl>,
    error: Option<StorageBackendError>,
}

impl Write for RetainedOutput<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if let Some(error) = self.original.and_then(|control| control.check().err()) {
            self.error = Some(error);
            return Err(std::io::Error::other("retained owner image cancelled"));
        }
        self.output.write(bytes)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.output.flush()
    }
}
