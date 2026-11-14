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

impl JsonDiagnostic {
    pub(in crate::json) fn wide_unsigned(value: u128, expected: &'static str) -> Self {
        Self {
            position: JsonPosition::unlocated(),
            cause: Cause::Type {
                expected,
                actual: Unexpected::WideUnsigned(value.to_le_bytes()),
            },
        }
    }

    pub(in crate::json) fn wide_signed(value: i128, expected: &'static str) -> Self {
        Self {
            position: JsonPosition::unlocated(),
            cause: Cause::Type {
                expected,
                actual: Unexpected::WideSigned(value.to_le_bytes()),
            },
        }
    }

    /// Type observation after flattened or untagged buffering retains the numeric visitor's actual classification instead of the generic value's arbitrary-precision number classification.
    pub async fn buffered_type_with_work<W: JsonReadWork<Error = JsonReadError>>(
        value: JsonValue<'_>,
        expected: &'static str,
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> Result<Self, JsonInputError> {
        use crate::json::document::project::numbers::{observe, Observation};
        control.check().map_err(JsonReadError::from)?;
        work.checkpoint().await?;
        control.check().map_err(JsonReadError::from)?;
        let Some(text) = value.number_text() else {
            return Self::projected_type_with_work(value, expected, control, work).await;
        };
        let actual = match observe(text) {
            Observation::Unsigned(value) => Unexpected::Unsigned(value),
            Observation::Signed(value) => Unexpected::Signed(value),
            Observation::WideUnsigned(value) => Unexpected::WideUnsigned(value.to_le_bytes()),
            Observation::WideSigned(value) => Unexpected::WideSigned(value.to_le_bytes()),
            Observation::Float(value) => Unexpected::Float(value),
            Observation::Map(_) => Unexpected::Map,
        };
        Ok(Self {
            position: JsonPosition::unlocated(),
            cause: Cause::Type { expected, actual },
        })
    }
}
