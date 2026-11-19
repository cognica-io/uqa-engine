//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Fixed metadata and coordinate windows retain validation and encoding positions.

use super::{
    invalid,
    layers::{fields, Field},
    validation_error, DeepModel, GatingSpec, ImagePreparation, ImageRead, StorageBackendResult,
    StorageReadControl, MAGIC,
};
use crate::{deep_fusion::ModelValidation, DeepLayerSpec};
use uqa_core::memory::MemoryError;

pub(super) struct ModelImageCursor<'a> {
    validation: ModelValidation<'a>,
    ready: bool,
    layers: std::slice::Iter<'a, DeepLayerSpec>,
    fields: [Field<'a>; 14],
    field: usize,
    bytes: &'a [u8],
    floats: std::slice::Iter<'a, f64>,
    pending: [u8; 4096],
    position: usize,
    length: usize,
    control: &'a StorageReadControl,
    original: Option<&'a StorageReadControl>,
}

impl<'a> ModelImageCursor<'a> {
    pub(super) fn new(
        model: &'a DeepModel,
        control: &'a StorageReadControl,
        original: Option<&'a StorageReadControl>,
    ) -> StorageBackendResult<Self> {
        control.check()?;
        if let Some(original) = original {
            original.check()?;
        }
        let validation =
            ModelValidation::model(&model.layers, model.alpha).map_err(validation_error)?;
        let mut pending = [0; 4096];
        pending[..8].copy_from_slice(MAGIC);
        pending[8..16].copy_from_slice(&model.alpha.to_bits().to_le_bytes());
        pending[16] = match model.gating {
            GatingSpec::None => 0,
            GatingSpec::ReLU => 1,
            GatingSpec::Swish => 2,
        };
        pending[17..25].copy_from_slice(&count(model.layers.len())?);
        Ok(Self {
            validation,
            ready: false,
            layers: model.layers.iter(),
            fields: [Field::End; 14],
            field: 0,
            bytes: &[],
            floats: [].iter(),
            pending,
            position: 0,
            length: 25,
            control,
            original,
        })
    }

    fn check(&self) -> StorageBackendResult<()> {
        self.control.check()?;
        self.original.map_or(Ok(()), StorageReadControl::check)
    }

    fn next_piece(&mut self) -> StorageBackendResult<bool> {
        self.check()?;
        self.position = 0;
        if !self.bytes.is_empty() {
            self.length = self.bytes.len().min(self.pending.len());
            self.pending[..self.length].copy_from_slice(&self.bytes[..self.length]);
            self.bytes = &self.bytes[self.length..];
        } else if !self.floats.as_slice().is_empty() {
            let count = self.floats.len().min(self.pending.len() / 8);
            for (target, value) in self
                .pending
                .chunks_exact_mut(8)
                .take(count)
                .zip(&mut self.floats)
            {
                target.copy_from_slice(&value.to_bits().to_le_bytes());
            }
            self.length = count * 8;
        } else {
            if self.field == self.fields.len() || matches!(self.fields[self.field], Field::End) {
                let Some(layer) = self.layers.next() else {
                    self.length = 0;
                    return Ok(false);
                };
                self.fields = fields(layer);
                self.field = 0;
            }
            let field = self.fields[self.field];
            self.field += 1;
            self.length = 8;
            let header = match field {
                Field::Byte(value) => {
                    self.pending[0] = value;
                    self.length = 1;
                    return Ok(true);
                }
                Field::Count(value) => count(value)?,
                Field::Float(value) => value.to_bits().to_le_bytes(),
                Field::Text(value) => {
                    self.bytes = value.as_bytes();
                    count(value.len())?
                }
                Field::Floats(values) => {
                    self.floats = values.iter();
                    count(values.len())?
                }
                Field::End => return Err(invalid()),
            };
            self.pending[..8].copy_from_slice(&header);
        }
        Ok(true)
    }
}

impl ImageRead for ModelImageCursor<'_> {
    fn prepare(&mut self, maximum_positions: usize) -> StorageBackendResult<ImagePreparation> {
        self.check()?;
        let mut positions = 0;
        while positions < maximum_positions && !self.ready {
            self.check()?;
            self.ready = self.validation.advance().map_err(validation_error)?;
            positions += 1;
        }
        self.check()?;
        Ok(ImagePreparation {
            positions,
            ready: self.ready,
        })
    }

    fn read(&mut self, output: &mut [u8]) -> StorageBackendResult<usize> {
        self.check()?;
        if output.is_empty() {
            return Ok(0);
        }
        if !self.ready {
            return Err(invalid());
        }
        let limit = output.len().min(16 * 1024);
        let mut written = 0;
        while written < limit {
            if self.position == self.length && !self.next_piece()? {
                break;
            }
            let count = (self.length - self.position).min(limit - written);
            output[written..written + count]
                .copy_from_slice(&self.pending[self.position..self.position + count]);
            self.position += count;
            written += count;
        }
        self.check()?;
        Ok(written)
    }
}

fn count(value: usize) -> StorageBackendResult<[u8; 8]> {
    Ok(u64::try_from(value)
        .map_err(|_| MemoryError::SizeOverflow)?
        .to_le_bytes())
}
