//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Read the stored array of label sequences into its native representation and compact JSON spelling.

use super::super::super::PathDeclaration;
use uqa_core::{
    image_io::AsyncImageInput,
    json::{decode_json_string_from, JsonDiagnostic, JsonInputError, JsonReader, JsonToken},
    memory::{Budgeted, BudgetedString, BudgetedVec, MemoryReservation, ProductionControl},
};
use uqa_storage::{StorageBackendError, StorageBackendResult};

pub(super) struct Definition {
    sequences: Vec<Vec<String>>,
    json: String,
    memory: MemoryReservation,
}

impl Definition {
    pub(super) fn bind(
        mut self,
        physical: Budgeted<String>,
        graph: Budgeted<String>,
    ) -> PathDeclaration {
        let (key, key_memory) = physical.into_parts();
        let (graph, graph_memory) = graph.into_parts();
        self.memory.absorb(key_memory);
        self.memory.absorb(graph_memory);
        PathDeclaration {
            key,
            graph,
            sequences: self.sequences,
            definition: self.json,
            _memory: Some(self.memory),
        }
    }
}

async fn start_array(
    reader: &mut JsonReader<'_, '_>,
    input: &mut AsyncImageInput<'_>,
) -> StorageBackendResult<()> {
    let event = reader
        .next_event_from(input)
        .await
        .map_err(|error| JsonInputError::from_reader(reader, error))?
        .ok_or(JsonInputError::Invalid)?;
    if event.token == JsonToken::StartArray {
        return Ok(());
    }
    Err(
        JsonDiagnostic::invalid_type_from(event.token, reader.position(), "a sequence", input)
            .await?
            .into(),
    )
}

async fn has_element(
    reader: &mut JsonReader<'_, '_>,
    input: &mut AsyncImageInput<'_>,
) -> StorageBackendResult<bool> {
    reader
        .next_array_value_with_work(input)
        .await
        .map_err(|error| JsonInputError::from_reader(reader, error).into())
}

async fn label(
    reader: &mut JsonReader<'_, '_>,
    input: &mut AsyncImageInput<'_>,
) -> StorageBackendResult<Budgeted<String>> {
    let event = reader
        .next_event_from(input)
        .await
        .map_err(|error| JsonInputError::from_reader(reader, error))?
        .ok_or(JsonInputError::Invalid)?;
    let JsonToken::String(encoded) = event.token else {
        return Err(JsonDiagnostic::invalid_type_from(
            event.token,
            reader.position(),
            "a string",
            input,
        )
        .await?
        .into());
    };
    decode_json_string_from(encoded, input)
        .await
        .map_err(|error| JsonInputError::from_reader(reader, error).into())
}

/// Preserve the original compact serde spelling even when a stored definition contains whitespace, Unicode escapes or escaped slashes.
async fn quote(
    label: &str,
    out: &mut BudgetedString,
    input: &mut AsyncImageInput<'_>,
) -> StorageBackendResult<()> {
    out.push_str("\"")?;
    let mut remaining = label;
    while !remaining.is_empty() {
        input.checkpoint().await?;
        let mut length = remaining.len().min(256);
        while !remaining.is_char_boundary(length) {
            length -= 1;
        }
        let (part, next) = remaining.split_at(length);
        append_escaped(part, out)?;
        remaining = next;
    }
    out.push_str("\"")?;
    Ok(())
}

fn append_escaped(part: &str, out: &mut BudgetedString) -> StorageBackendResult<()> {
    // A JSON string fragment grows by at most six bytes per input byte and two surrounding quotes.
    let mut buffer = [0; 256 * 6 + 2];
    let mut output = std::io::Cursor::new(&mut buffer[..]);
    serde_json::to_writer(&mut output, part)?;
    let end = usize::try_from(output.position()).expect("fixed JSON fragment");
    let escaped =
        std::str::from_utf8(&buffer[1..end - 1]).expect("the JSON serializer emits UTF-8");
    out.push_str(escaped)?;
    Ok(())
}

pub(super) async fn read(
    text: &str,
    input: &mut AsyncImageInput<'_>,
) -> StorageBackendResult<Definition> {
    let control = ProductionControl::new(input.memory, input.cancellation, input.cancellation);
    let mut reader = JsonReader::with_control(text, &control).with_depth_limit(127);
    start_array(&mut reader, input).await?;
    let mut outer = BudgetedVec::new(input.memory);
    let mut output = BudgetedString::new(input.memory);
    let mut memory = input.memory.empty_reservation();
    output.push_str("[")?;
    while has_element(&mut reader, input).await? {
        start_array(&mut reader, input).await?;
        output.push_str(if outer.is_empty() { "[" } else { ",[" })?;
        let mut sequence = BudgetedVec::new(input.memory);
        while has_element(&mut reader, input).await? {
            let value = label(&mut reader, input).await?;
            if !sequence.is_empty() {
                output.push_str(",")?;
            }
            quote(&value, &mut output, input).await?;
            let (value, value_memory) = value.into_parts();
            sequence.push(value)?;
            memory.absorb(value_memory);
        }
        output.push_str("]")?;
        let (sequence, sequence_memory) = sequence.into_parts();
        outer.push(sequence)?;
        memory.absorb(sequence_memory);
    }
    output.push_str("]")?;
    if reader
        .next_event_from(input)
        .await
        .map_err(|error| JsonInputError::from_reader(&reader, error))?
        .is_some()
    {
        return Err(StorageBackendError::from(JsonInputError::Invalid));
    }
    let (sequences, slots) = outer.into_parts();
    let (json, json_memory) = output.into_parts();
    memory.absorb(slots);
    memory.absorb(json_memory);
    Ok(Definition {
        sequences,
        json,
        memory,
    })
}

#[cfg(test)]
mod tests;
