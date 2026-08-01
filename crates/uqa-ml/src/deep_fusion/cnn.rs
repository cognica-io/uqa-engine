//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! One- and two-dimensional convolution kernels.

use super::{
    apply_gating, runtime_filled_vec, runtime_model_error, runtime_vec_with_capacity, BTreeMap,
    Convolution1D, Convolution2D, ForwardState, Gating, StorageBackendResult,
};

pub(super) fn apply_cnn_1d(
    params: Convolution1D<'_>,
    gating: Gating,
    state: &mut ForwardState,
) -> StorageBackendResult<()> {
    let input_ids: Vec<u64> = state.channel_map.keys().copied().collect();
    if input_ids.is_empty() {
        return Ok(());
    }
    for doc_id in &input_ids {
        let channels = state.channel_map[doc_id].len();
        if channels != params.input_channels {
            return Err(runtime_model_error(format!(
                "CNN1D input for doc {doc_id} has {channels} channels, expected {}",
                params.input_channels
            )));
        }
    }
    let double_padding = params
        .padding
        .checked_mul(2)
        .ok_or_else(|| runtime_model_error("CNN1D padding overflows usize"))?;
    let padded_len = input_ids
        .len()
        .checked_add(double_padding)
        .ok_or_else(|| runtime_model_error("CNN1D padded input length overflows usize"))?;
    if padded_len < params.kernel_size {
        state.channel_map.clear();
        state.num_channels = params.output_channels;
        state.softmax_applied = false;
        return Ok(());
    }
    let output_len = (padded_len - params.kernel_size) / params.stride + 1;
    let mut next_synthetic_id = input_ids.last().and_then(|doc_id| doc_id.checked_add(1));
    let mut output = BTreeMap::new();
    for out_pos in 0..output_len {
        let mut row = runtime_filled_vec(params.output_channels, 0.0f64, "CNN1D output channels")?;
        for (out_ch, slot) in row.iter_mut().enumerate() {
            let mut acc = params.bias[out_ch];
            for kernel_pos in 0..params.kernel_size {
                let raw_pos = out_pos
                    .checked_mul(params.stride)
                    .and_then(|position| position.checked_add(kernel_pos))
                    .ok_or_else(|| runtime_model_error("CNN1D window position overflows usize"))?;
                if raw_pos < params.padding {
                    continue;
                }
                let input_pos = raw_pos - params.padding;
                if input_pos >= input_ids.len() {
                    continue;
                }
                let input_vec = &state.channel_map[&input_ids[input_pos]];
                for (in_ch, input_value) in input_vec.iter().enumerate() {
                    let weight_index =
                        (out_ch * params.kernel_size + kernel_pos) * params.input_channels + in_ch;
                    acc += params.weights[weight_index] * input_value;
                }
            }
            *slot = apply_gating(acc, gating);
        }
        let doc_id = if let Some(doc_id) = input_ids.get(out_pos).copied() {
            doc_id
        } else {
            let doc_id = next_synthetic_id
                .ok_or_else(|| runtime_model_error("CNN1D output document IDs exceed u64"))?;
            next_synthetic_id = doc_id.checked_add(1);
            doc_id
        };
        output.insert(doc_id, row);
    }
    state.channel_map = output;
    state.num_channels = params.output_channels;
    state.softmax_applied = false;
    Ok(())
}

pub(super) fn apply_cnn_2d(
    params: Convolution2D<'_>,
    gating: Gating,
    state: &mut ForwardState,
) -> StorageBackendResult<()> {
    let input_ids: Vec<u64> = state.channel_map.keys().copied().collect();
    if input_ids.is_empty() {
        return Ok(());
    }
    let expected_input = params
        .input_height
        .checked_mul(params.input_width)
        .and_then(|size| size.checked_mul(params.input_channels))
        .ok_or_else(|| runtime_model_error("CNN2D input size overflows usize"))?;
    let mut flat_input = runtime_vec_with_capacity(expected_input, "CNN2D flattened input")?;
    if input_ids.len() == 1 {
        flat_input.extend_from_slice(&state.channel_map[&input_ids[0]]);
    } else {
        for doc_id in &input_ids {
            flat_input.extend_from_slice(&state.channel_map[doc_id]);
        }
    }

    if flat_input.len() != expected_input {
        return Err(runtime_model_error(format!(
            "CNN2D input has {} scalar values, expected {expected_input}",
            flat_input.len()
        )));
    }

    let padded_height = params
        .padding_height
        .checked_mul(2)
        .and_then(|padding| params.input_height.checked_add(padding))
        .ok_or_else(|| runtime_model_error("CNN2D padded height overflows usize"))?;
    let padded_width = params
        .padding_width
        .checked_mul(2)
        .and_then(|padding| params.input_width.checked_add(padding))
        .ok_or_else(|| runtime_model_error("CNN2D padded width overflows usize"))?;
    if padded_height < params.kernel_height || padded_width < params.kernel_width {
        state.channel_map.clear();
        state.num_channels = params.output_channels;
        state.softmax_applied = false;
        return Ok(());
    }
    let output_height = (padded_height - params.kernel_height) / params.stride_height + 1;
    let output_width = (padded_width - params.kernel_width) / params.stride_width + 1;
    let mut next_synthetic_id = input_ids.last().and_then(|doc_id| doc_id.checked_add(1));
    let mut output = BTreeMap::new();

    for out_row in 0..output_height {
        for out_col in 0..output_width {
            let mut row =
                runtime_filled_vec(params.output_channels, 0.0f64, "CNN2D output channels")?;
            for (out_ch, slot) in row.iter_mut().enumerate() {
                *slot = apply_gating(
                    cnn_2d_cell(&params, &flat_input, out_row, out_col, out_ch)?,
                    gating,
                );
            }
            let output_index = out_row
                .checked_mul(output_width)
                .and_then(|index| index.checked_add(out_col))
                .ok_or_else(|| runtime_model_error("CNN2D output index overflows usize"))?;
            let doc_id = if let Some(doc_id) = input_ids.get(output_index).copied() {
                doc_id
            } else {
                let doc_id = next_synthetic_id
                    .ok_or_else(|| runtime_model_error("CNN2D output document IDs exceed u64"))?;
                next_synthetic_id = doc_id.checked_add(1);
                doc_id
            };
            output.insert(doc_id, row);
        }
    }
    state.channel_map = output;
    state.num_channels = params.output_channels;
    state.softmax_applied = false;
    Ok(())
}

pub(super) fn cnn_2d_cell(
    params: &Convolution2D<'_>,
    flat_input: &[f64],
    out_row: usize,
    out_col: usize,
    out_ch: usize,
) -> StorageBackendResult<f64> {
    let mut acc = params.bias[out_ch];
    for kernel_row in 0..params.kernel_height {
        let raw_row = out_row
            .checked_mul(params.stride_height)
            .and_then(|row| row.checked_add(kernel_row))
            .ok_or_else(|| runtime_model_error("CNN2D row position overflows usize"))?;
        if raw_row < params.padding_height {
            continue;
        }
        let input_row = raw_row - params.padding_height;
        if input_row >= params.input_height {
            continue;
        }
        for kernel_col in 0..params.kernel_width {
            let raw_col = out_col
                .checked_mul(params.stride_width)
                .and_then(|column| column.checked_add(kernel_col))
                .ok_or_else(|| runtime_model_error("CNN2D column position overflows usize"))?;
            if raw_col < params.padding_width {
                continue;
            }
            let input_col = raw_col - params.padding_width;
            if input_col >= params.input_width {
                continue;
            }
            for in_ch in 0..params.input_channels {
                let input_index =
                    ((input_row * params.input_width + input_col) * params.input_channels) + in_ch;
                let weight_index = (((out_ch * params.kernel_height + kernel_row)
                    * params.kernel_width
                    + kernel_col)
                    * params.input_channels)
                    + in_ch;
                acc += params.weights[weight_index] * flat_input[input_index];
            }
        }
    }
    Ok(acc)
}
