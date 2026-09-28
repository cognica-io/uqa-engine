//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Reuse Core's borrowed grammar before any protocol value materialization.

use super::{ProtocolError, MAX_NOTIFICATION_WIRE_BYTES};
use serde::Serialize;
use std::io::{self, Write};
use uqa_core::{
    json::{JsonReader, JsonToken},
    memory::ProductionControl,
};

pub(super) fn validate(input: &[u8]) -> Result<&str, ProtocolError> {
    if input.len() > MAX_NOTIFICATION_WIRE_BYTES {
        return Err(ProtocolError::ByteLimit);
    }
    let input = std::str::from_utf8(input).map_err(|_| ProtocolError::InvalidUTF8)?;
    let mut reader =
        JsonReader::with_control(input, &ProductionControl::uncontrolled()).with_depth_limit(2);
    if !matches!(
        reader
            .next_event()
            .map_err(|_| ProtocolError::InvalidJSON)?
            .map(|event| event.token),
        Some(JsonToken::StartObject)
    ) {
        return Err(ProtocolError::InvalidJSON);
    }
    while reader
        .next_event()
        .map_err(|_| ProtocolError::InvalidJSON)?
        .is_some()
    {}
    Ok(input)
}

pub(super) fn encode(value: &impl Serialize, retain: bool) -> Result<Vec<u8>, ProtocolError> {
    let mut writer = Writer {
        bytes: retain.then(Vec::new),
        length: 0,
        failure: None,
    };
    if let Some(bytes) = &mut writer.bytes {
        bytes
            .try_reserve_exact(MAX_NOTIFICATION_WIRE_BYTES)
            .map_err(|_| ProtocolError::Allocation)?;
    }
    serde_json::to_writer(&mut writer, value)
        .map_err(|_| writer.failure.unwrap_or(ProtocolError::InvalidJSON))?;
    Ok(writer.bytes.unwrap_or_default())
}

struct Writer {
    bytes: Option<Vec<u8>>,
    length: usize,
    failure: Option<ProtocolError>,
}

impl Write for Writer {
    fn write(&mut self, input: &[u8]) -> io::Result<usize> {
        let Some(length) = self
            .length
            .checked_add(input.len())
            .filter(|length| *length <= MAX_NOTIFICATION_WIRE_BYTES)
        else {
            self.failure = Some(ProtocolError::ByteLimit);
            return Err(io::Error::other("notification JSON byte limit"));
        };
        if let Some(bytes) = &mut self.bytes {
            bytes.extend_from_slice(input);
        }
        self.length = length;
        Ok(input.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
