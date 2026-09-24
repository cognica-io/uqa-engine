//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! String tokens use the shared grammar and admit their decoded UTF-8 output before production.

use super::{Budgeted, CancellationToken, JsonReadError, JsonReader, JsonToken, MemoryBudget};
use crate::memory::{Produced, ProductionControl, ProductionString};

/// Decode one quoted JSON string under a shared allowance, retaining its actual destination capacity.
pub fn decode_json_string(
    encoded: impl AsRef<[u8]>,
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<Budgeted<String>, JsonReadError> {
    decode_json_string_with_control(
        encoded,
        &ProductionControl::new(memory, cancellation, cancellation),
    )?
    .into_budgeted()
    .map_err(|_| unreachable!("controlled JSON string"))
}

/// Decode with the existing token grammar and one admitted string constructor. Both cancellation owners are checked during validation and output; no parser-owned escape scratch or second output string is allocated.
pub fn decode_json_string_with_control(
    encoded: impl AsRef<[u8]>,
    control: &ProductionControl<'_>,
) -> Result<Produced<String>, JsonReadError> {
    control.check()?;
    let input = std::str::from_utf8(encoded.as_ref()).map_err(|_| JsonReadError::InvalidJson)?;
    let mut reader = JsonReader::with_control(input, control);
    let event = reader.next_event()?.ok_or(JsonReadError::InvalidJson)?;
    let JsonToken::String(token) = event.token else {
        return Err(JsonReadError::InvalidJson);
    };
    if reader.next_event()?.is_some() {
        return Err(JsonReadError::InvalidJson);
    }
    let text = std::str::from_utf8(token).map_err(|_| JsonReadError::InvalidJson)?;
    let mut output = ProductionString::new(*control);
    let mut position = 1;
    while position < text.len() - 1 {
        control.check()?;
        let start = position;
        while position < text.len() - 1 && text.as_bytes()[position] != b'\\' {
            position += 1;
            if position.is_multiple_of(4096) {
                control.check()?;
            }
        }
        output.push_str(&text[start..position])?;
        if position == text.len() - 1 {
            break;
        }
        position += 1;
        let escape = text.as_bytes()[position];
        position += 1;
        let character = match escape {
            b'"' => '"',
            b'\\' => '\\',
            b'/' => '/',
            b'b' => '\u{0008}',
            b'f' => '\u{000c}',
            b'n' => '\n',
            b'r' => '\r',
            b't' => '\t',
            b'u' => {
                let mut code = hex_unit(text, &mut position);
                if (0xd800..=0xdbff).contains(&code) {
                    position += 2;
                    let low = hex_unit(text, &mut position);
                    code = 0x10000 + ((code - 0xd800) << 10) + (low - 0xdc00);
                }
                char::from_u32(code).expect("validated JSON Unicode escape")
            }
            _ => unreachable!("validated JSON escape"),
        };
        output.push(character)?;
    }
    output.finish().map_err(Into::into)
}

fn hex_unit(text: &str, position: &mut usize) -> u32 {
    let value = u32::from_str_radix(&text[*position..*position + 4], 16)
        .expect("validated JSON hexadecimal escape");
    *position += 4;
    value
}
