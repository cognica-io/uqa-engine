//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Inline maintenance images preserve the captured counters and sampled publication identity.

use super::StatisticsMaintenance;
use std::io::{Read, Write};
use uqa_core::{
    image_io::{
        decode_image, AsyncImageInput, ImageDecoder, ImageIoError, ImageIoResult, ImageOutput,
    },
    memory::MemoryBudget,
    CancellationToken,
};

#[cfg(test)]
mod tests;

const MAGIC: [u8; 8] = *b"UQASMT01";
const LENGTH: usize = 88;

fn put<const N: usize>(bytes: &mut [u8; LENGTH], offset: &mut usize, value: [u8; N]) {
    bytes[*offset..*offset + N].copy_from_slice(&value);
    *offset += N;
}

fn optional<const N: usize>(bytes: &mut [u8; LENGTH], offset: &mut usize, value: Option<[u8; N]>) {
    put(bytes, offset, [u8::from(value.is_some())]);
    put(bytes, offset, value.unwrap_or([0; N]));
}

async fn read_optional<const N: usize>(
    input: &mut AsyncImageInput<'_>,
) -> ImageIoResult<Option<[u8; N]>> {
    let flag = input.array::<1>().await?[0];
    let bytes = input.array().await?;
    match flag {
        0 if bytes == [0; N] => Ok(None),
        1 => Ok(Some(bytes)),
        _ => Err(ImageIoError::Invalid),
    }
}

enum State {
    Active,
    Complete,
    Failed,
}

/// Fixed inline output requires no payload allocation. Failure permanently retires the cursor; output must remain private until the enclosing image is complete.
pub struct StatisticsMaintenanceImageReader<'a> {
    bytes: [u8; LENGTH],
    offset: usize,
    cancellation: &'a CancellationToken,
    state: State,
}

impl StatisticsMaintenance {
    /// Capture all inline fields without recomputing counters, clocks or analysis eligibility.
    pub fn image_reader<'a>(
        &self,
        cancellation: &'a CancellationToken,
    ) -> ImageIoResult<StatisticsMaintenanceImageReader<'a>> {
        cancellation.check()?;
        let mut bytes = [0; LENGTH];
        let mut offset = 0;
        put(&mut bytes, &mut offset, MAGIC);
        put(&mut bytes, &mut offset, 1u64.to_le_bytes());
        optional(&mut bytes, &mut offset, self.object_id);
        put(&mut bytes, &mut offset, self.generation.to_le_bytes());
        put(&mut bytes, &mut offset, self.changes.to_le_bytes());
        put(&mut bytes, &mut offset, self.dirty_since_ms.to_le_bytes());
        optional(
            &mut bytes,
            &mut offset,
            self.analyzed_rows.map(u64::to_le_bytes),
        );
        put(
            &mut bytes,
            &mut offset,
            self.statistics_format.to_le_bytes(),
        );
        optional(
            &mut bytes,
            &mut offset,
            self.analyzed_at.map(u64::to_le_bytes),
        );
        optional(
            &mut bytes,
            &mut offset,
            self.analysis_generation.map(u64::to_le_bytes),
        );
        debug_assert_eq!(offset, LENGTH);
        Ok(StatisticsMaintenanceImageReader {
            bytes,
            offset: 0,
            cancellation,
            state: State::Active,
        })
    }

    /// Read complete inline maintenance state without allocating or publishing it. The enclosing relation restore validates the table identity and statistics format.
    pub fn read_image(
        reader: &mut dyn Read,
        memory: &MemoryBudget,
        cancellation: &CancellationToken,
    ) -> ImageIoResult<Self> {
        decode_image(reader, memory, cancellation, async |input| {
            Self::read_image_from(input).await
        })
    }

    /// Resume inline fields within the caller's current byte/poll window; no partially decoded maintenance state is published.
    pub fn image_input(
        memory: &MemoryBudget,
        cancellation: &CancellationToken,
    ) -> ImageIoResult<ImageDecoder<Self>> {
        ImageDecoder::new(memory, cancellation, |mut reader| async move {
            Self::read_image_from(reader.input()).await
        })
    }

    async fn read_image_from(mut input: AsyncImageInput<'_>) -> ImageIoResult<Self> {
        if input.array::<8>().await? != MAGIC || u64::from_le_bytes(input.array().await?) != 1 {
            return Err(ImageIoError::Invalid);
        }
        let value = Self {
            object_id: read_optional(&mut input).await?,
            generation: u64::from_le_bytes(input.array().await?),
            changes: u64::from_le_bytes(input.array().await?),
            dirty_since_ms: u64::from_le_bytes(input.array().await?),
            analyzed_rows: read_optional(&mut input).await?.map(u64::from_le_bytes),
            statistics_format: u32::from_le_bytes(input.array().await?),
            analyzed_at: read_optional(&mut input).await?.map(u64::from_le_bytes),
            analysis_generation: read_optional(&mut input).await?.map(u64::from_le_bytes),
        };
        input.end().await?;
        Ok(value)
    }
}

impl StatisticsMaintenanceImageReader<'_> {
    pub fn read(&mut self, output: &mut [u8]) -> ImageIoResult<usize> {
        match std::mem::replace(&mut self.state, State::Failed) {
            State::Failed => return Err(ImageIoError::Invalid),
            State::Complete => {
                self.state = State::Complete;
                return Ok(0);
            }
            State::Active => {}
        }
        self.cancellation.check()?;
        let count = output.len().min(LENGTH - self.offset);
        output[..count].copy_from_slice(&self.bytes[self.offset..self.offset + count]);
        self.offset += count;
        self.state = if self.offset == LENGTH {
            State::Complete
        } else {
            State::Active
        };
        Ok(count)
    }

    pub fn write_to(&mut self, writer: &mut dyn Write) -> ImageIoResult<u64> {
        match self.state {
            State::Failed => return Err(ImageIoError::Invalid),
            State::Complete => return Ok(0),
            State::Active => {}
        }
        self.state = State::Failed;
        let mut output = ImageOutput {
            writer,
            cancellation: self.cancellation,
            written: 0,
        };
        output.bytes(&self.bytes[self.offset..])?;
        self.offset = LENGTH;
        self.state = State::Complete;
        Ok(output.written)
    }
}
