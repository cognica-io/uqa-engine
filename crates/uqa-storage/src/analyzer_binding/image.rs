//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A field binding retains both original revisions independently of current named resources.

use super::{
    capture::CapturedFieldAnalyzerBinding, AnalyzerBindingOwner, BoundAnalyzerRevision,
    FieldAnalyzerBinding,
};
use crate::{
    analyzer_image::read_analyzer_image_from,
    image_io::{framed_image_reader, read_framed_image_from, ImageOwner, ImageRead, ImageReader},
    read_control::StorageReadControl,
    AnalyzerPhase, StorageBackendError, StorageBackendResult,
};
use std::{io::Read, sync::Arc};
use uqa_analysis::AnalyzerResources;
use uqa_core::{
    image_io::{decode_image, AsyncImageInput},
    memory::{Budgeted, MemoryReservation},
};

const MAGIC: &[u8; 8] = b"UQAFAB01";

fn invalid() -> StorageBackendError {
    StorageBackendError::Other("invalid field analyzer binding image".into())
}

impl CapturedFieldAnalyzerBinding {
    /// Borrow the original names, ownership and both resolved analyzer images. Fixed framing scratch is independent of name, descriptor and dictionary sizes; both original cancellation scopes remain active through metadata and child output.
    pub fn image_reader<'a>(
        &'a self,
        control: &'a StorageReadControl,
    ) -> StorageBackendResult<ImageReader<'a>> {
        let mut header = [0; 10];
        header[..8].copy_from_slice(MAGIC);
        header[8] = match self.owner {
            AnalyzerBindingOwner::Field => 0,
            AnalyzerBindingOwner::Gin => 1,
        };
        header[9] = match self.last_phase {
            AnalyzerPhase::Index => 0,
            AnalyzerPhase::Search => 1,
            AnalyzerPhase::Both => 2,
        };
        ImageReader::new(
            Output {
                binding: self,
                control,
                header,
                length: 10,
                offset: 0,
                text: &[],
                stage: 0,
                child: None,
            },
            control,
        )?
        .with_source_control(Some(self.index.image.read_control()), control)?
        .with_source_control(Some(self.search.image.read_control()), control)
    }
}

impl ImageOwner for Arc<Budgeted<CapturedFieldAnalyzerBinding>> {
    fn image_reader<'a>(
        &'a self,
        control: &'a StorageReadControl,
    ) -> StorageBackendResult<ImageReader<'a>> {
        CapturedFieldAnalyzerBinding::image_reader(self, control)
    }
}

struct Output<'a> {
    binding: &'a CapturedFieldAnalyzerBinding,
    control: &'a StorageReadControl,
    header: [u8; 10],
    length: usize,
    offset: usize,
    text: &'a [u8],
    stage: u8,
    child: Option<ImageReader<'a>>,
}

impl<'a> Output<'a> {
    fn name(&mut self, value: Option<&'a str>) {
        self.header[0] = u8::from(value.is_some());
        self.length = 1;
        if let Some(value) = value {
            self.header[1..9].copy_from_slice(&(value.len() as u64).to_le_bytes());
            self.length = 9;
            self.text = value.as_bytes();
        }
    }

    fn advance(&mut self) -> StorageBackendResult<bool> {
        self.offset = 0;
        self.length = 0;
        match self.stage {
            0 => self.name(self.binding.index.name.as_deref()),
            1 => self.name(self.binding.search.name.as_deref()),
            2 | 3 => {
                let image = if self.stage == 2 {
                    &self.binding.index.image
                } else {
                    &self.binding.search.image
                };
                self.child = Some(framed_image_reader(
                    image.image_reader(self.control)?,
                    self.control,
                )?);
            }
            _ => return Ok(false),
        }
        self.stage += 1;
        Ok(true)
    }
}

impl ImageRead for Output<'_> {
    fn read(&mut self, output: &mut [u8]) -> StorageBackendResult<usize> {
        let mut written = 0;
        while written < output.len() {
            if self.offset < self.length {
                let count = (self.length - self.offset).min(output.len() - written);
                output[written..written + count]
                    .copy_from_slice(&self.header[self.offset..self.offset + count]);
                self.offset += count;
                written += count;
            } else if !self.text.is_empty() {
                let count = self.text.len().min(output.len() - written);
                output[written..written + count].copy_from_slice(&self.text[..count]);
                self.text = &self.text[count..];
                written += count;
            } else if let Some(child) = &mut self.child {
                let count = child.read(&mut output[written..])?;
                written += count;
                if count == 0 {
                    self.child = None;
                }
            } else if !self.advance()? {
                break;
            }
        }
        Ok(written)
    }
}

async fn name(
    input: &mut AsyncImageInput<'_>,
    memory: &mut MemoryReservation,
) -> StorageBackendResult<Option<String>> {
    match input.array::<1>().await?[0] {
        0 => Ok(None),
        1 => {
            let (value, lease) = input.text().await?.into_parts();
            memory.absorb(lease);
            Ok(Some(value))
        }
        _ => Err(invalid()),
    }
}

impl FieldAnalyzerBinding {
    /// Reconstruct private index/search revisions solely from their supplied descriptor and dictionary images, then apply the existing binding invariants. Names and encoded artifacts retain their input allowances; decoded analyzer resources use their destination owner's limits.
    pub fn read_image(
        reader: &mut dyn Read,
        resources: &AnalyzerResources,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Budgeted<Self>> {
        decode_image(
            reader,
            control.memory(),
            control.cancellation(),
            async |mut input| Self::read_image_from(&mut input, resources).await,
        )
    }

    /// Retain both framed analyzer revisions and their original name allocations through the caller's byte/poll channel. Only complete validated bindings are returned; failed or abandoned preparation retires both private revisions.
    pub async fn read_image_from(
        input: &mut AsyncImageInput<'_>,
        resources: &AnalyzerResources,
    ) -> StorageBackendResult<Budgeted<Self>> {
        input.cancellation.check()?;
        let mut memory = input.memory.reserve(size_of::<Self>())?;
        if &input.array::<8>().await? != MAGIC {
            return Err(invalid());
        }
        let owner = match input.array::<1>().await?[0] {
            0 => AnalyzerBindingOwner::Field,
            1 => AnalyzerBindingOwner::Gin,
            _ => return Err(invalid()),
        };
        let last_phase = match input.array::<1>().await?[0] {
            0 => AnalyzerPhase::Index,
            1 => AnalyzerPhase::Search,
            2 => AnalyzerPhase::Both,
            _ => return Err(invalid()),
        };
        let index_name = name(input, &mut memory).await?;
        let search_name = name(input, &mut memory).await?;
        let index = read_framed_image_from(input, async |mut child| {
            read_analyzer_image_from(&mut child, resources).await
        })
        .await?;
        let search = read_framed_image_from(input, async |mut child| {
            read_analyzer_image_from(&mut child, resources).await
        })
        .await?;
        input.end().await?;
        let value = Self {
            index: BoundAnalyzerRevision {
                name: index_name,
                compiled: index,
            },
            search: BoundAnalyzerRevision {
                name: search_name,
                compiled: search,
            },
            owner,
            last_phase,
        };
        value.validate()?;
        input.cancellation.check()?;
        Ok(Budgeted::new(value, memory))
    }
}

#[cfg(test)]
mod tests;
