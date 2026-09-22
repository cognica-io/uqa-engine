//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native JSONB construction consumes the shared JSON grammar with its own number and duplicate-key semantics.

use crate::json::JsonToken;

use super::{
    workspace::{ParseBuffer, Workspace},
    JsonbField, JsonbKeyError, JsonbValue,
};

pub(super) struct JsonbParser;

enum Container {
    Array(ParseBuffer<JsonbValue>),
    Object {
        fields: ParseBuffer<JsonbField>,
        key: Option<(String, usize)>,
    },
}

impl JsonbParser {
    pub(super) fn parse(input: &str) -> Option<JsonbValue> {
        let mut workspace = Workspace::unbounded();
        Self::parse_with(input, &mut workspace).ok()
    }

    pub(super) fn parse_with(
        input: &str,
        workspace: &mut Workspace<'_>,
    ) -> Result<JsonbValue, JsonbKeyError> {
        workspace.check()?;
        let mut reader = workspace.reader(input);
        let mut stack = workspace.buffer();
        let mut root = None;
        while let Some(event) = reader.next_event()? {
            let value = match event.token {
                JsonToken::Null => JsonbValue::Null,
                JsonToken::Bool(value) => JsonbValue::Bool(value),
                JsonToken::Number(text) => JsonbValue::Number(workspace.number(text)?),
                JsonToken::String(text) => JsonbValue::String(workspace.string(text)?),
                JsonToken::Key(text) => {
                    let Some(Container::Object { key, .. }) = stack.last_mut() else {
                        unreachable!("JSON object key event");
                    };
                    *key = Some((workspace.string(text)?, event.range.start));
                    continue;
                }
                JsonToken::StartArray => {
                    stack.push(Container::Array(workspace.buffer()))?;
                    continue;
                }
                JsonToken::StartObject => {
                    stack.push(Container::Object {
                        fields: workspace.buffer(),
                        key: None,
                    })?;
                    continue;
                }
                JsonToken::EndArray => {
                    let Some(Container::Array(values)) = stack.pop() else {
                        unreachable!("JSON array end event");
                    };
                    JsonbValue::Array(values.finish(workspace))
                }
                JsonToken::EndObject => {
                    let Some(Container::Object { fields, key: None }) = stack.pop() else {
                        unreachable!("JSON object end event");
                    };
                    let mut fields = fields.finish(workspace);
                    // Keep the last occurrence of each key, then retain lexical key order for the native equality representation. Sorting needs no temporary allocation.
                    fields.sort_unstable_by(|left, right| {
                        left.name
                            .cmp(&right.name)
                            .then_with(|| right.position.cmp(&left.position))
                    });
                    fields.dedup_by(|later, earlier| later.name == earlier.name);
                    JsonbValue::Object(fields)
                }
            };
            match stack.last_mut() {
                Some(Container::Array(values)) => values.push(value)?,
                Some(Container::Object { fields, key }) => {
                    let (name, position) = key.take().expect("JSON value follows its object key");
                    fields.push(JsonbField {
                        name,
                        value,
                        position,
                    })?;
                }
                None => root = Some(value),
            }
        }
        workspace.check()?;
        root.ok_or(JsonbKeyError::InvalidJson)
    }
}
