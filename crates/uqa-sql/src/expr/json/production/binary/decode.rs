//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stream retained JSONB output without input validation or object-key sorting.

use super::{
    walk::{Event, Input, Scalar, Stream},
    Produced, ProductionControl, Result, SQLError,
};
use crate::expr::json::production::writer;
use uqa_core::memory::ProductionString;

pub(super) fn decode(bytes: &[u8], control: &ProductionControl<'_>) -> Result<Produced<String>> {
    let mut output = ProductionString::new(*control);
    let mut stream = Stream::new(Input::Bytes(bytes), control)?;
    while let Some(event) = stream.next(control)? {
        match event {
            Event::Begin(container, prefix) => {
                output.push_str(prefix)?;
                if container.object {
                    output.push('{')?;
                } else if !container.scalar {
                    output.push('[')?;
                }
            }
            Event::End(container) => {
                if container.object {
                    output.push('}')?;
                } else if !container.scalar {
                    output.push(']')?;
                }
            }
            Event::Scalar(value, prefix) => {
                output.push_str(prefix)?;
                match value {
                    Scalar::Null => output.push_str("null")?,
                    Scalar::Bool(value) => output.push_str(if value { "true" } else { "false" })?,
                    Scalar::Number(number) => output.push_str(&number.text(control)?)?,
                    Scalar::String(span) => {
                        let bytes = span.bytes()?;
                        let text =
                            std::str::from_utf8(bytes).map_err(|error| SQLError::Routine {
                                sqlstate: "22021".into(),
                                message: format!(
                                    "invalid byte sequence for encoding \"UTF8\": 0x{:02x}",
                                    bytes[error.valid_up_to()]
                                ),
                            })?;
                        writer::write_string(text, &mut output, control)?;
                    }
                }
            }
        }
    }
    Ok(output.finish()?)
}
