//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native decoders borrow one admitted address and its verified payload together.

use super::{
    BackupError, BackupImageReader, BackupResult, MemoryImageImportReader, MemoryImageImportUnit,
};
use std::io::Read;

struct UnitReader<'r, 'a> {
    image: &'r mut BackupImageReader<'a>,
    failure: Option<BackupError>,
}

impl Read for UnitReader<'_, '_> {
    fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
        if self.failure.is_some() {
            return Err(std::io::Error::other("retired native image input"));
        }
        match self.image.read_current(output) {
            Ok(count) => Ok(count),
            Err(error) => {
                self.failure = Some(error);
                Err(std::io::Error::other("native image payload read failed"))
            }
        }
    }
}

impl MemoryImageImportReader<'_> {
    /// Run an owning native codec against the current component without copying its address or materializing its bytes. Successful decoding requires verified EOF. Codec errors, payload errors and unwinding retire the complete input; repository errors retain their original typed cause.
    pub fn decode_current<T>(
        &mut self,
        decode: impl FnOnce(MemoryImageImportUnit<'_>, &mut dyn Read) -> BackupResult<T>,
    ) -> BackupResult<T> {
        Ok(self
            .decode_current_step(|unit, reader| decode(unit, reader).map(Some))?
            .expect("a complete native codec always returns its output"))
    }

    /// Retain the same admitted component across owning decoder steps. `None` keeps decoding active, including after payload EOF; only `Some` certifies codec completion and verified EOF. `advance` keeps returning the same component while decoding is pending; switching to raw reads retires the input. Errors and unwinding retire the original input exactly as for `decode_current`.
    pub fn decode_current_step<T>(
        &mut self,
        decode: impl FnOnce(MemoryImageImportUnit<'_>, &mut dyn Read) -> BackupResult<Option<T>>,
    ) -> BackupResult<Option<T>> {
        if self.complete {
            return Err(BackupError::Finished);
        }
        let Some(address) = self.current.take() else {
            self.release(false);
            return Err(BackupError::Finished);
        };
        let Some(mut image) = self.reader.take() else {
            self.release(false);
            return Err(BackupError::Finished);
        };
        let header = self.header.take();
        self.previous = None;
        let decoded_bytes = image
            .current_unit()
            .expect("admitted memory component")
            .decoded_bytes();
        self.verified_bytes = image.verified_object_bytes();
        let mut input = UnitReader {
            image: &mut image,
            failure: None,
        };
        let result = (|| {
            self.control.check()?;
            decode(
                MemoryImageImportUnit {
                    address: &address,
                    decoded_bytes,
                },
                &mut input,
            )
        })();
        let result = match input.failure.take() {
            Some(error) => Err(error),
            None => result,
        }
        .and_then(|value| {
            if value.is_some() {
                image.require_current_end()?;
            }
            Ok(value)
        });
        match result {
            Ok(value) => {
                self.reader = Some(image);
                self.current = Some(address);
                self.header = header;
                self.decoding = value.is_none();
                Ok(value)
            }
            Err(error) => {
                self.release(false);
                Err(error)
            }
        }
    }
}
