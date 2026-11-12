//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stored graph identities retain the original strict JSON shapes and map replacement order.

use super::{GraphCatalogOids, LabelCatalogOids};
use crate::SQLError;
use uqa_core::{
    image_io::AsyncImageInput,
    json::{JsonReadError, JsonReader, JsonToken},
    memory::{Budgeted, OwnedMap, ProductionControl},
};

mod fields;
mod label;
use fields::{invalid, Fields, Input};

/// Decode borrowed persistent JSON through the caller's retained work channel. Every returned buffer keeps its original allowance; this decoder does not allocate or regenerate catalog OIDs.
pub async fn decode(
    text: &str,
    control: &ProductionControl<'_>,
    work: &mut AsyncImageInput<'_>,
) -> Result<Budgeted<GraphCatalogOids>, SQLError> {
    control.check()?;
    if control.budget().is_none() {
        return Err(SQLError::Internal(
            "stored graph input requires admitted production".into(),
        ));
    }
    let mut input = Input {
        reader: JsonReader::with_control(text, control).with_depth_limit(127),
        control: *control,
        work,
    };
    let first = input.next().await?;
    let mut fields = Fields::new(first, &["namespace", "label_sequence", "labels"], 7)?;
    let (mut namespace, mut label_sequence, mut labels) = (0, 0, None);
    while let Some((field, token)) = fields.next(&mut input).await? {
        match field {
            0 => namespace = fields::number(token)?,
            1 => label_sequence = fields::number(token)?,
            2 => labels = Some(input.labels(token).await?),
            _ => unreachable!(),
        }
    }
    if input
        .reader
        .next_event_from(input.work)
        .await
        .map_err(read_error)?
        .is_some()
    {
        return Err(invalid());
    }
    control.check()?;
    let (labels, memory) = labels.ok_or_else(invalid)?.into_parts();
    Ok(Budgeted::new(
        GraphCatalogOids {
            namespace,
            label_sequence,
            labels,
        },
        memory,
    ))
}

impl Input<'_, '_, '_, '_> {
    async fn labels(
        &mut self,
        first: JsonToken<'_, &[u8]>,
    ) -> Result<Budgeted<OwnedMap<u32, LabelCatalogOids>>, SQLError> {
        if first != JsonToken::StartObject {
            return Err(invalid());
        }
        let mut memory = self.memory().empty_reservation();
        let mut labels = OwnedMap::<u32, LabelCatalogOids>::new();
        loop {
            let key = self.next().await?;
            if key == JsonToken::EndObject {
                break;
            }
            let JsonToken::Key(key) = key else {
                return Err(invalid());
            };
            // JSON's numeric map keys use the unescaped decimal token, not the decoded string carrier.
            let key = fields::number(JsonToken::Number(&key[1..key.len() - 1]))?;
            let first = self.next().await?;
            let label = self.label(first).await?;
            let previous_bytes = if let Some(previous) = labels.get(&key) {
                self.label_bytes(previous).await?
            } else {
                memory.grow(OwnedMap::<u32, LabelCatalogOids>::entry_bytes())?;
                0
            };
            let (label, payload) = label.into_parts();
            memory.absorb(payload);
            drop(labels.insert(key, label));
            // The replaced native allocations are gone before their original lease is released.
            drop(memory.split(previous_bytes));
        }
        Ok(Budgeted::new(labels, memory))
    }

    async fn label_bytes(&mut self, label: &LabelCatalogOids) -> Result<usize, SQLError> {
        let mut bytes = label.not_null.allocated_bytes();
        for name in label.not_null.keys() {
            self.work.checkpoint().await?;
            bytes = bytes
                .checked_add(name.capacity())
                .ok_or(uqa_core::memory::MemoryError::SizeOverflow)?;
        }
        Ok(bytes)
    }
}

fn read_error(error: JsonReadError) -> SQLError {
    match error {
        JsonReadError::Memory(error) => error.into(),
        JsonReadError::Cancelled(error) => error.into(),
        JsonReadError::InvalidJson | JsonReadError::Malformed { .. } => invalid(),
    }
}

#[cfg(test)]
mod tests;
