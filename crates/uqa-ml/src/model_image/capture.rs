//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Immutable stored models retain their actual buffer capacities without duplicating weights.

use super::{write_model_image, DeepModel, StorageBackendResult, StorageReadControl};
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
        let Self {
            layers,
            alpha: _,
            gating: _,
        } = self.as_ref();
        let mut memory = control
            .memory()
            .reserve(size_of::<Self>() + size_of::<RetainedModel>())?;
        buffer::<DeepLayerSpec>(&mut memory, layers.capacity())?;
        for layer in layers {
            control.check()?;
            retain_layer(layer, &mut memory)?;
        }
        control.check()?;
        Ok(ModelImageSnapshot(Arc::new(RetainedModel {
            source: self.clone(),
            control: control.clone(),
            _memory: memory,
        })))
    }
}

impl ModelImageSnapshot {
    pub fn write_image(
        &self,
        output: &mut dyn std::io::Write,
        control: &StorageReadControl,
    ) -> StorageBackendResult<u64> {
        uqa_storage::image_io::with_retained_control(output, Some(&self.0.control), |output| {
            write_model_image(&self.0.source, output, control)
        })
    }
}

fn buffer<T>(memory: &mut MemoryReservation, capacity: usize) -> StorageBackendResult<()> {
    memory.grow(
        capacity
            .checked_mul(size_of::<T>())
            .ok_or(MemoryError::SizeOverflow)?,
    )?;
    Ok(())
}

fn retain_layer(layer: &DeepLayerSpec, memory: &mut MemoryReservation) -> StorageBackendResult<()> {
    match layer {
        DeepLayerSpec::Input { dimensions: _ }
        | DeepLayerSpec::Flatten
        | DeepLayerSpec::GlobalPool { method: _ }
        | DeepLayerSpec::Softmax
        | DeepLayerSpec::BatchNorm { epsilon: _ }
        | DeepLayerSpec::Dropout { p: _ }
        | DeepLayerSpec::Attention => {}
        DeepLayerSpec::Embed { embedding } => buffer::<f64>(memory, embedding.capacity())?,
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
            buffer::<f64>(memory, weights.capacity())?;
            buffer::<f64>(memory, bias.capacity())?;
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
        } => memory.grow(edge_label.capacity())?,
        DeepLayerSpec::Conv {
            edge_label,
            hop_weights,
            direction: _,
        } => {
            memory.grow(edge_label.capacity())?;
            buffer::<f64>(memory, hop_weights.capacity())?;
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
                buffer::<f64>(memory, weights.capacity())?;
            }
        }
    }
    Ok(())
}
