//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Immutable stored models retain their actual buffer capacities without duplicating weights.

use super::{write_parts, DeepModel, ImageReader, StorageBackendResult, StorageReadControl};
use crate::DeepLayerSpec;
use std::sync::Arc;
use uqa_core::memory::{MemoryError, MemoryReservation};

struct RetainedModel {
    source: Arc<DeepModel>,
    control: StorageReadControl,
    _memory: MemoryReservation,
}

/// An export-only model owner with independent source retention and original capture cancellation.
#[derive(Clone)]
pub struct ModelImageSnapshot(Arc<RetainedModel>);

impl DeepModel {
    pub fn capture_image_with_control(
        self: &Arc<Self>,
        control: &StorageReadControl,
    ) -> StorageBackendResult<ModelImageSnapshot> {
        control.check()?;
        let bytes = retained_model_bytes(self, || control.check())?;
        let mut memory = control.memory().reserve(bytes)?;
        memory.grow(ModelImageSnapshot::wrapper_bytes())?;
        control.check()?;
        Ok(ModelImageSnapshot::from_admitted(
            self.clone(),
            control,
            memory,
        ))
    }
}

impl ModelImageSnapshot {
    /// Borrow this exact model's bounded validator and output continuation.
    pub fn image_reader<'a>(
        &'a self,
        control: &'a StorageReadControl,
    ) -> StorageBackendResult<ImageReader<'a>> {
        self.0.control.check()?;
        ImageReader::new(
            super::stream::ModelImageCursor::new(&self.0.source, control, Some(&self.0.control))?,
            control,
        )
    }

    /// Transfer the retained model into an independent movable reader without copying its weights.
    pub fn into_image_reader(
        self,
        control: &StorageReadControl,
    ) -> StorageBackendResult<ImageReader<'static>> {
        ImageReader::from_owner(self, control)
    }

    pub(crate) fn wrapper_bytes() -> usize {
        size_of::<RetainedModel>()
    }

    pub(crate) fn from_admitted(
        source: Arc<DeepModel>,
        control: &StorageReadControl,
        memory: MemoryReservation,
    ) -> Self {
        Self(Arc::new(RetainedModel {
            source,
            control: control.clone(),
            _memory: memory,
        }))
    }

    pub fn write_image(
        &self,
        output: &mut dyn std::io::Write,
        control: &StorageReadControl,
    ) -> StorageBackendResult<u64> {
        uqa_storage::image_io::with_retained_control(output, Some(&self.0.control), |output| {
            write_parts(&self.0.source, output, control, Some(&self.0.control))
        })
    }
}

impl uqa_storage::image_io::ImageOwner for ModelImageSnapshot {
    fn image_reader<'a>(
        &'a self,
        control: &'a StorageReadControl,
    ) -> StorageBackendResult<ImageReader<'a>> {
        self.image_reader(control)
    }
}

pub(crate) fn retained_model_bytes<E: From<MemoryError>>(
    model: &DeepModel,
    mut check: impl FnMut() -> Result<(), E>,
) -> Result<usize, E> {
    let DeepModel {
        layers,
        alpha: _,
        gating: _,
    } = model;
    let mut bytes = size_of::<DeepModel>();
    buffer::<DeepLayerSpec>(&mut bytes, layers.capacity())?;
    for layer in layers {
        check()?;
        retain_layer(layer, &mut bytes)?;
    }
    Ok(bytes)
}

fn buffer<T>(bytes: &mut usize, capacity: usize) -> Result<(), MemoryError> {
    add(
        bytes,
        capacity
            .checked_mul(size_of::<T>())
            .ok_or(MemoryError::SizeOverflow)?,
    )
}

fn add(bytes: &mut usize, additional: usize) -> Result<(), MemoryError> {
    *bytes = bytes
        .checked_add(additional)
        .ok_or(MemoryError::SizeOverflow)?;
    Ok(())
}

fn retain_layer(layer: &DeepLayerSpec, bytes: &mut usize) -> Result<(), MemoryError> {
    match layer {
        DeepLayerSpec::Input { dimensions: _ }
        | DeepLayerSpec::Flatten
        | DeepLayerSpec::GlobalPool { method: _ }
        | DeepLayerSpec::Softmax
        | DeepLayerSpec::BatchNorm { epsilon: _ }
        | DeepLayerSpec::Dropout { p: _ }
        | DeepLayerSpec::Attention => {}
        DeepLayerSpec::Embed { embedding } => buffer::<f64>(bytes, embedding.capacity())?,
        DeepLayerSpec::Dense {
            weights,
            bias,
            output_channels: _,
            input_channels: _,
        }
        | DeepLayerSpec::CNN1D {
            weights,
            bias,
            output_channels: _,
            input_channels: _,
            kernel_size: _,
            stride: _,
            padding: _,
        }
        | DeepLayerSpec::CNN2D {
            weights,
            bias,
            output_channels: _,
            input_channels: _,
            input_height: _,
            input_width: _,
            kernel_height: _,
            kernel_width: _,
            stride_height: _,
            stride_width: _,
            padding_height: _,
            padding_width: _,
        } => {
            buffer::<f64>(bytes, weights.capacity())?;
            buffer::<f64>(bytes, bias.capacity())?;
        }
        DeepLayerSpec::Propagate {
            edge_label,
            aggregation: _,
            direction: _,
        }
        | DeepLayerSpec::Pool {
            edge_label,
            pool_size: _,
            method: _,
            direction: _,
        } => add(bytes, edge_label.capacity())?,
        DeepLayerSpec::Conv {
            edge_label,
            hop_weights,
            direction: _,
        } => {
            add(bytes, edge_label.capacity())?;
            buffer::<f64>(bytes, hop_weights.capacity())?;
        }
        DeepLayerSpec::RNN {
            weights_input,
            weights_hidden,
            bias,
            hidden_channels: _,
            input_channels: _,
            return_sequences: _,
        }
        | DeepLayerSpec::LSTM {
            weights_input,
            weights_hidden,
            bias,
            hidden_channels: _,
            input_channels: _,
            return_sequences: _,
        } => {
            for weights in [weights_input, weights_hidden, bias] {
                buffer::<f64>(bytes, weights.capacity())?;
            }
        }
    }
    Ok(())
}
