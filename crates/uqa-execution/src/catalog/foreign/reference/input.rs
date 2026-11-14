//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Observe the stored JSON marker without allocating discarded values or numeric lexemes.

use serde::de::Error as _;
use uqa_core::{
    json::{decode_json_string_with_control, JsonReadError, JsonReader, JsonToken},
    memory::{BudgetedSmallVec, ProductionControl},
    CancellationToken,
};
use uqa_storage::{
    catalog::input::CatalogInputLimits, read_control::StorageReadControl, CatalogFacade,
    StorageBackendError, StorageBackendResult,
};

pub(crate) fn check_format_with_control(
    catalog: &dyn CatalogFacade,
    limits: CatalogInputLimits,
    control: &StorageReadControl,
    invoking: &CancellationToken,
) -> StorageBackendResult<Option<u32>> {
    let production = ProductionControl::new(control.memory(), control.cancellation(), invoking);
    production.check()?;
    let Some(marker) = catalog.get_metadata_with_control(super::FORMAT_KEY, limits, control)?
    else {
        return Ok(None);
    };
    match observe(&marker, production)? {
        Shape::Marker(version) => Ok(Some(version)),
        _ => Err(super::invalid(
            "unsupported foreign-table server-reference format marker",
        )),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Shape {
    Other,
    Integer(u32),
    Marker(u32),
}

enum Frame {
    Array,
    Object {
        first: bool,
        version_key: bool,
        other_key: bool,
        version: Shape,
    },
    Carrier {
        raw: bool,
        value: Option<Shape>,
    },
}

impl Frame {
    fn accept(&mut self, value: Shape) {
        match self {
            Self::Array => (),
            Self::Object {
                version_key,
                version,
                ..
            } => {
                if *version_key {
                    *version = value;
                }
            }
            Self::Carrier { value: slot, .. } => *slot = Some(value),
        }
    }

    fn finish(self) -> Shape {
        match self {
            Self::Object {
                other_key: false,
                version: Shape::Integer(version),
                ..
            } => Shape::Marker(version),
            Self::Carrier {
                value: Some(value), ..
            } => value,
            _ => Shape::Other,
        }
    }
}

fn observe(text: &str, control: ProductionControl<'_>) -> StorageBackendResult<Shape> {
    let mut reader = JsonReader::with_control(text, &control).with_depth_limit(127);
    let mut frames =
        BudgetedSmallVec::<[Frame; 16]>::new(control.budget().expect("controlled format marker"));
    let mut root = None;
    while let Some(event) = reader.next_event().map_err(json_error)? {
        if let JsonToken::Key(key) = event.token {
            let key = decode_json_string_with_control(key, &control).map_err(json_error)?;
            let Some(frame) = frames.last_mut() else {
                return Err(invalid());
            };
            let Frame::Object {
                first,
                version_key,
                other_key,
                ..
            } = frame
            else {
                return Err(invalid());
            };
            // These are persisted serde_json Value carriers, recognized only as the first key.
            if *first
                && matches!(
                    key.as_str(),
                    "$serde_json::private::Number" | "$serde_json::private::RawValue"
                )
            {
                *frame = Frame::Carrier {
                    raw: key.as_str() == "$serde_json::private::RawValue",
                    value: None,
                };
            } else {
                *first = false;
                *version_key = key.as_str() == "version";
                *other_key |= !*version_key;
            }
            continue;
        }
        if let Some(Frame::Carrier { raw, value: None }) = frames.last() {
            let JsonToken::String(encoded) = event.token else {
                return Err(invalid());
            };
            let text = decode_json_string_with_control(encoded, &control).map_err(json_error)?;
            let value = if *raw {
                // Raw carriers parse a fresh JSON document, with its own ordinary nesting limit.
                observe(&text, control)?
            } else {
                let mut number = JsonReader::with_control(&text, &control);
                let event = number
                    .next_event()
                    .map_err(json_error)?
                    .ok_or_else(invalid)?;
                let JsonToken::Number(value) = event.token else {
                    return Err(invalid());
                };
                if event.range != (0..text.len())
                    || number.next_event().map_err(json_error)?.is_some()
                {
                    return Err(invalid());
                }
                integer(value)
            };
            frames.last_mut().expect("carrier frame").accept(value);
            continue;
        }
        let value = match event.token {
            JsonToken::StartArray => {
                frames.push(Frame::Array)?;
                continue;
            }
            JsonToken::StartObject => {
                frames.push(Frame::Object {
                    first: true,
                    version_key: false,
                    other_key: false,
                    version: Shape::Other,
                })?;
                continue;
            }
            JsonToken::EndArray | JsonToken::EndObject => {
                frames.pop().ok_or_else(invalid)?.finish()
            }
            JsonToken::Number(value) => integer(value),
            JsonToken::Null | JsonToken::Bool(_) | JsonToken::String(_) => Shape::Other,
            JsonToken::Key(_) => unreachable!("keys handled above"),
        };
        if let Some(frame) = frames.last_mut() {
            frame.accept(value);
        } else {
            root = Some(value);
        }
    }
    control.check()?;
    root.ok_or_else(invalid)
}

fn integer(text: &str) -> Shape {
    match text {
        "1" => Shape::Integer(1),
        "2" => Shape::Integer(2),
        _ => Shape::Other,
    }
}

fn invalid() -> StorageBackendError {
    StorageBackendError::Serde(serde_json::Error::custom(
        "invalid foreign-table server-reference format JSON",
    ))
}

fn json_error(error: JsonReadError) -> StorageBackendError {
    match error {
        JsonReadError::Memory(error) => error.into(),
        JsonReadError::Cancelled(error) => error.into(),
        _ => invalid(),
    }
}

#[cfg(test)]
mod tests;
