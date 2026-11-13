//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Cause, JsonDiagnostic, JsonInputError, JsonPosition, Unexpected};
use crate::{
    json::{JsonReadError, JsonReadWork, JsonValue, JsonValueKind},
    memory::{ProductionControl, ProductionString},
};

impl JsonDiagnostic {
    /// Type errors after generic-value projection have no physical location. Number failures retain the arbitrary-precision value classification; strings retain admitted decoded payloads.
    pub async fn projected_type_with_work<W: JsonReadWork<Error = JsonReadError>>(
        value: JsonValue<'_>,
        expected: &'static str,
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> Result<Self, JsonInputError> {
        let actual = match value.kind() {
            JsonValueKind::Null => Unexpected::Null,
            JsonValueKind::Boolean => Unexpected::Bool(value.as_bool().expect("JSON boolean")),
            JsonValueKind::Number => Unexpected::Number,
            JsonValueKind::Array => Unexpected::Sequence,
            JsonValueKind::Object => Unexpected::Map,
            JsonValueKind::String => {
                let text = value.as_str().expect("JSON string");
                let mut output = ProductionString::new(*control);
                let mut offset = 0;
                while offset < text.len() {
                    work.checkpoint().await?;
                    let mut end = (offset + 4096).min(text.len());
                    while !text.is_char_boundary(end) {
                        end -= 1;
                    }
                    output
                        .push_str(&text[offset..end])
                        .map_err(JsonReadError::from)?;
                    offset = end;
                }
                Unexpected::String(output.finish().map_err(JsonReadError::from)?)
            }
        };
        control.check().map_err(JsonReadError::from)?;
        Ok(Self {
            position: JsonPosition::unlocated(),
            cause: Cause::Type { expected, actual },
        })
    }
}
