//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! String tokens use the shared grammar and admit their decoded UTF-8 output before production.

use super::work::{self, Work};
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
    work::inline(decode(encoded.as_ref(), *control, &mut ()))
}

/// Decode the same quoted token through checkpoints without reading the image source. The result keeps the original input allowance; literal runs retain the synchronous constructor's capacity decisions.
pub async fn decode_json_string_from(
    encoded: &[u8],
    input: &mut crate::image_io::AsyncImageInput<'_>,
) -> Result<Budgeted<String>, JsonReadError> {
    let control = ProductionControl::new(input.memory, input.cancellation, input.cancellation);
    decode(encoded, control, input)
        .await?
        .into_budgeted()
        .map_err(|_| unreachable!("controlled JSON string"))
}

/// Decode a quoted token using the enclosing scheduler and the supplied original allowance.
pub async fn decode_json_string_with_work<W: Work>(
    encoded: &[u8],
    control: &ProductionControl<'_>,
    work: &mut W,
) -> Result<Produced<String>, W::Error> {
    decode(encoded, *control, work).await
}

async fn decode<W: Work>(
    encoded: &[u8],
    control: ProductionControl<'_>,
    work: &mut W,
) -> Result<Produced<String>, W::Error> {
    control.check().map_err(JsonReadError::from)?;
    let mut reader = JsonReader::from_slice_with_control(encoded, &control);
    let event = reader
        .next_event_with_work(work)
        .await?
        .ok_or(JsonReadError::InvalidJson)?;
    let JsonToken::String(token) = event.token else {
        return Err(JsonReadError::InvalidJson.into());
    };
    if reader.next_event_with_work(work).await?.is_some() {
        return Err(JsonReadError::InvalidJson.into());
    }
    let mut output = ProductionString::new(control);
    let mut position = 1;
    while position < token.len() - 1 {
        control.check().map_err(JsonReadError::from)?;
        work.checkpoint().await?;
        let start = position;
        while position < token.len() - 1 && token[position] != b'\\' {
            position += 1;
            if position.is_multiple_of(4096) {
                control.check().map_err(JsonReadError::from)?;
                work.checkpoint().await?;
            }
        }
        copy_literal(&token[start..position], &mut output, work).await?;
        if position == token.len() - 1 {
            break;
        }
        position += 1;
        let escape = token[position];
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
                let mut code = hex_unit(token, &mut position);
                if (0xd800..=0xdbff).contains(&code) {
                    position += 2;
                    let low = hex_unit(token, &mut position);
                    code = 0x10000 + ((code - 0xd800) << 10) + (low - 0xdc00);
                }
                char::from_u32(code).expect("validated JSON Unicode escape")
            }
            _ => unreachable!("validated JSON escape"),
        };
        output.push(character).map_err(JsonReadError::from)?;
    }
    output
        .finish()
        .map_err(JsonReadError::from)
        .map_err(Into::into)
}

async fn copy_literal<W: Work>(
    mut bytes: &[u8],
    output: &mut ProductionString<'_>,
    work: &mut W,
) -> Result<(), W::Error> {
    while !bytes.is_empty() {
        work.checkpoint().await?;
        let length = bytes.len().min(4096);
        let text = match std::str::from_utf8(&bytes[..length]) {
            Ok(text) => text,
            Err(error) if error.error_len().is_none() && length < bytes.len() => {
                std::str::from_utf8(&bytes[..error.valid_up_to()]).expect("validated UTF-8 prefix")
            }
            Err(_) => return Err(JsonReadError::InvalidJson.into()),
        };
        output.push_str(text).map_err(JsonReadError::from)?;
        bytes = &bytes[text.len()..];
    }
    Ok(())
}

fn hex_unit(bytes: &[u8], position: &mut usize) -> u32 {
    let text = std::str::from_utf8(&bytes[*position..*position + 4])
        .expect("validated hexadecimal digits");
    let value = u32::from_str_radix(text, 16).expect("validated JSON hexadecimal escape");
    *position += 4;
    value
}
