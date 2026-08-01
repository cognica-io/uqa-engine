//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Forward-pass and typed kernel parameter state.

use super::BTreeMap;

pub(super) struct ForwardState {
    pub(super) channel_map: BTreeMap<u64, Vec<f64>>,
    pub(super) num_channels: usize,
    pub(super) softmax_applied: bool,
}

#[derive(Clone, Copy)]
pub(super) struct Convolution1D<'a> {
    pub(super) weights: &'a [f64],
    pub(super) bias: &'a [f64],
    pub(super) output_channels: usize,
    pub(super) input_channels: usize,
    pub(super) kernel_size: usize,
    pub(super) stride: usize,
    pub(super) padding: usize,
}

#[derive(Clone, Copy)]
pub(super) struct Convolution2D<'a> {
    pub(super) weights: &'a [f64],
    pub(super) bias: &'a [f64],
    pub(super) output_channels: usize,
    pub(super) input_channels: usize,
    pub(super) input_height: usize,
    pub(super) input_width: usize,
    pub(super) kernel_height: usize,
    pub(super) kernel_width: usize,
    pub(super) stride_height: usize,
    pub(super) stride_width: usize,
    pub(super) padding_height: usize,
    pub(super) padding_width: usize,
}

#[derive(Clone, Copy)]
pub(super) struct Recurrent<'a> {
    pub(super) weights_input: &'a [f64],
    pub(super) weights_hidden: &'a [f64],
    pub(super) bias: &'a [f64],
    pub(super) hidden_channels: usize,
    pub(super) input_channels: usize,
    pub(super) return_sequences: bool,
}

#[derive(Clone, Copy)]
pub(super) struct LongShortTermMemory<'a> {
    pub(super) weights_input: &'a [f64],
    pub(super) weights_hidden: &'a [f64],
    pub(super) bias: &'a [f64],
    pub(super) hidden_channels: usize,
    pub(super) input_channels: usize,
    pub(super) return_sequences: bool,
}
