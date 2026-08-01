//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! RNN and LSTM kernels.

use super::{
    runtime_filled_vec, runtime_model_error, sigmoid, BTreeMap, ForwardState, LongShortTermMemory,
    Recurrent, StorageBackendResult,
};

pub(super) fn apply_rnn(
    params: Recurrent<'_>,
    state: &mut ForwardState,
) -> StorageBackendResult<()> {
    let input_ids: Vec<u64> = state.channel_map.keys().copied().collect();
    if input_ids.is_empty() {
        return Ok(());
    }
    let mut hidden = runtime_filled_vec(params.hidden_channels, 0.0f64, "RNN hidden state")?;
    let mut output = BTreeMap::new();
    for doc_id in &input_ids {
        let input = &state.channel_map[doc_id];
        if input.len() != params.input_channels {
            return Err(runtime_model_error(format!(
                "RNN input for doc {doc_id} has {} channels, expected {}",
                input.len(),
                params.input_channels
            )));
        }
        let mut next_hidden =
            runtime_filled_vec(params.hidden_channels, 0.0f64, "RNN next hidden state")?;
        for (out_ch, slot) in next_hidden.iter_mut().enumerate() {
            let mut acc = params.bias[out_ch];
            for (in_ch, input_value) in input.iter().enumerate() {
                acc += params.weights_input[out_ch * params.input_channels + in_ch] * input_value;
            }
            for (hidden_ch, hidden_value) in hidden.iter().enumerate() {
                acc += params.weights_hidden[out_ch * params.hidden_channels + hidden_ch]
                    * hidden_value;
            }
            *slot = acc.tanh();
        }
        hidden = next_hidden;
        if params.return_sequences {
            let output_hidden = crate::backend::try_clone_slice(&hidden, "RNN output state")
                .map_err(|error| runtime_model_error(error.to_string()))?;
            output.insert(*doc_id, output_hidden);
        }
    }
    if !params.return_sequences {
        let last_doc_id = input_ids
            .last()
            .copied()
            .ok_or_else(|| runtime_model_error("RNN input unexpectedly became empty"))?;
        output.insert(last_doc_id, hidden);
    }
    state.channel_map = output;
    state.num_channels = params.hidden_channels;
    state.softmax_applied = false;
    Ok(())
}

pub(super) fn apply_lstm(
    params: LongShortTermMemory<'_>,
    state: &mut ForwardState,
) -> StorageBackendResult<()> {
    let gate_channels = params
        .hidden_channels
        .checked_mul(4)
        .ok_or_else(|| runtime_model_error("LSTM gate channel count overflows usize"))?;
    let input_ids: Vec<u64> = state.channel_map.keys().copied().collect();
    if input_ids.is_empty() {
        return Ok(());
    }
    let mut hidden = runtime_filled_vec(params.hidden_channels, 0.0f64, "LSTM hidden state")?;
    let mut cell = runtime_filled_vec(params.hidden_channels, 0.0f64, "LSTM cell state")?;
    let mut output = BTreeMap::new();
    for doc_id in &input_ids {
        let input = &state.channel_map[doc_id];
        if input.len() != params.input_channels {
            return Err(runtime_model_error(format!(
                "LSTM input for doc {doc_id} has {} channels, expected {}",
                input.len(),
                params.input_channels
            )));
        }
        let mut gates = runtime_filled_vec(gate_channels, 0.0f64, "LSTM gate channels")?;
        for (gate_ch, gate_slot) in gates.iter_mut().enumerate() {
            let mut acc = params.bias[gate_ch];
            for (in_ch, input_value) in input.iter().enumerate() {
                acc += params.weights_input[gate_ch * params.input_channels + in_ch] * input_value;
            }
            for (hidden_ch, hidden_value) in hidden.iter().enumerate() {
                acc += params.weights_hidden[gate_ch * params.hidden_channels + hidden_ch]
                    * hidden_value;
            }
            *gate_slot = acc;
        }

        let mut next_hidden =
            runtime_filled_vec(params.hidden_channels, 0.0f64, "LSTM next hidden state")?;
        let mut next_cell =
            runtime_filled_vec(params.hidden_channels, 0.0f64, "LSTM next cell state")?;
        for ch in 0..params.hidden_channels {
            let input_gate = sigmoid(gates[ch]);
            let forget_gate = sigmoid(gates[params.hidden_channels + ch]);
            let candidate = gates[2 * params.hidden_channels + ch].tanh();
            let output_gate = sigmoid(gates[3 * params.hidden_channels + ch]);
            next_cell[ch] = forget_gate * cell[ch] + input_gate * candidate;
            next_hidden[ch] = output_gate * next_cell[ch].tanh();
        }
        hidden = next_hidden;
        cell = next_cell;
        if params.return_sequences {
            let output_hidden = crate::backend::try_clone_slice(&hidden, "LSTM output state")
                .map_err(|error| runtime_model_error(error.to_string()))?;
            output.insert(*doc_id, output_hidden);
        }
    }
    if !params.return_sequences {
        let last_doc_id = input_ids
            .last()
            .copied()
            .ok_or_else(|| runtime_model_error("LSTM input unexpectedly became empty"))?;
        output.insert(last_doc_id, hidden);
    }
    state.channel_map = output;
    state.num_channels = params.hidden_channels;
    state.softmax_applied = false;
    Ok(())
}
