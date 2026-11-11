//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Child grammars consume exactly one canonical frame stream through the parent's current window.

use super::{io, FrameState};
use uqa_core::image_io::{
    decode_image_from, AsyncImageInput, AsyncImageRead, ImageIoError, ImageIoResult,
};

/// Decode one framed child without consuming its parent's following fields. The caller's admitted continuation owns framing and child parsing; empty, partial and interrupted transport use the same canonical framing rules as synchronous input.
pub async fn read_framed_image_from<T, E, F>(
    source: &mut AsyncImageInput<'_>,
    create: F,
) -> Result<T, E>
where
    E: From<ImageIoError>,
    F: for<'a> AsyncFnOnce(AsyncImageInput<'a>) -> Result<T, E>,
{
    let memory = source.memory;
    let cancellation = source.cancellation;
    let mut input = FramedInput {
        source,
        state: Some(FrameState::default()),
    };
    let value = decode_image_from(&mut input, memory, cancellation, create).await?;
    if input.read(&mut [0; 1]).await? != 0 {
        return Err(ImageIoError::Invalid.into());
    }
    Ok(value)
}

struct FramedInput<'a, 'b> {
    source: &'a mut AsyncImageInput<'b>,
    state: Option<FrameState>,
}

impl AsyncImageRead for FramedInput<'_, '_> {
    async fn read(&mut self, output: &mut [u8]) -> ImageIoResult<usize> {
        let mut state = self.state.take().ok_or(ImageIoError::Invalid)?;
        let result = state.read_from(self.source, output).await;
        if match &result {
            Ok(_) => true,
            Err(ImageIoError::Io(error)) => error.kind() == io::ErrorKind::Interrupted,
            Err(_) => false,
        } {
            self.state = Some(state);
        }
        result
    }

    async fn checkpoint(&mut self) -> ImageIoResult<()> {
        self.source.checkpoint().await
    }
}

impl FrameState {
    async fn read_from(
        &mut self,
        source: &mut AsyncImageInput<'_>,
        output: &mut [u8],
    ) -> ImageIoResult<usize> {
        if output.is_empty() || self.complete {
            return Ok(0);
        }
        if self.remaining == 0 {
            while self.header_used < 4 {
                let count = source.read(&mut self.header[self.header_used..]).await?;
                self.accept_header_bytes(count)?;
            }
            self.finish_header()?;
            if self.complete {
                return Ok(0);
            }
        }
        let limit = output.len().min(self.remaining);
        let count = source.read(&mut output[..limit]).await?;
        Ok(self.accept_payload(count, limit)?)
    }
}
