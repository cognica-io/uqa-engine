//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Evaluated values retain the same native decoder across input windows.

use super::{read::Reader, ValueImageError, ValueImageResult, MAGIC};
use crate::{
    image_io::{AsyncImageInput, ImageDecodeWindow, ImageDecoder, ImageIoError},
    memory::{Budgeted, MemoryBudget},
    CancellationToken, Value,
};
use std::io::Read;

impl From<ImageIoError> for ValueImageError {
    fn from(error: ImageIoError) -> Self {
        match error {
            ImageIoError::Invalid => Self::Invalid("owner image stream"),
            ImageIoError::Memory(error) => Self::Memory(error),
            ImageIoError::Cancelled(error) => Self::Cancelled(error),
            ImageIoError::Io(error) => Self::Io(error),
        }
    }
}

/// Resumable native value input. Each advance limits actual source bytes and input continuation steps; value-specific post-input construction and validation retain their existing owner boundaries. The returned value keeps its original payload allowances after the decoder retires.
pub struct ValueImageInput(ImageDecoder<Budgeted<Value>, ValueImageError>);

impl ValueImageInput {
    pub fn new(memory: &MemoryBudget, cancellation: &CancellationToken) -> ValueImageResult<Self> {
        Ok(Self(ImageDecoder::new(
            memory,
            cancellation,
            |mut reader| async move { Self::read_from(&mut reader.input()).await },
        )?))
    }

    pub fn advance(
        &mut self,
        source: &mut dyn Read,
        window: ImageDecodeWindow,
    ) -> ValueImageResult<Option<Budgeted<Value>>> {
        self.0.advance(source, window)
    }

    pub fn read(self, source: &mut dyn Read) -> ValueImageResult<Budgeted<Value>> {
        self.0.read(source)
    }

    /// Decode exactly one value from an enclosing native continuation without opening a second stream or retaining a second read buffer.
    pub async fn read_from(input: &mut AsyncImageInput<'_>) -> ValueImageResult<Budgeted<Value>> {
        let memory = input.memory;
        let cancellation = input.cancellation;
        let mut reader = Reader {
            input,
            memory,
            cancellation,
        };
        if reader.fixed::<8>().await? != *MAGIC {
            return Err(ValueImageError::Invalid("version header"));
        }
        let value = reader.value(0).await?;
        cancellation.check()?;
        Ok(value)
    }
}
