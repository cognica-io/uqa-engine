//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{inline, Separators, Work, WINDOW};
use crate::StorageBackendResult;
use uqa_core::{
    image_io::AsyncImageInput,
    memory::{Budgeted, BudgetedString, MemoryBudget, MemoryError},
};

// Encoded addresses never contain a literal double colon; every historical paired address does.
const PREFIX: &str = "@uqa-path-1/";
const HEX: &[u8; 16] = b"0123456789abcdef";

async fn ambiguous(graph: &str, name: &str, work: &mut impl Work) -> StorageBackendResult<bool> {
    if graph.ends_with(':') || name.starts_with(':') {
        return Ok(true);
    }
    for raw in [graph, name] {
        for candidate in (Separators { raw, offset: 0 }) {
            work.checkpoint().await?;
            if candidate.is_some() {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

trait Buffer {
    fn append(&mut self, text: &str) -> Result<(), MemoryError>;
}
impl Buffer for String {
    fn append(&mut self, text: &str) -> Result<(), MemoryError> {
        self.push_str(text);
        Ok(())
    }
}
impl Buffer for BudgetedString {
    fn append(&mut self, text: &str) -> Result<(), MemoryError> {
        self.push_str(text)
    }
}

async fn component(
    value: &str,
    hex: bool,
    buffer: &mut impl Buffer,
    work: &mut impl Work,
) -> StorageBackendResult<()> {
    let mut remaining = value;
    while !remaining.is_empty() {
        work.checkpoint().await?;
        let mut end = remaining.len().min(WINDOW);
        while !remaining.is_char_boundary(end) {
            end -= 1;
        }
        let (chunk, tail) = remaining.split_at(end);
        if hex {
            let mut encoded = [0; WINDOW * 2];
            for (byte, pair) in chunk.bytes().zip(encoded.chunks_exact_mut(2)) {
                pair.copy_from_slice(&[HEX[usize::from(byte >> 4)], HEX[usize::from(byte & 15)]]);
            }
            buffer.append(
                std::str::from_utf8(&encoded[..chunk.len() * 2])
                    .expect("hexadecimal digits are ASCII"),
            )?;
        } else {
            buffer.append(chunk)?;
        }
        remaining = tail;
    }
    Ok(())
}

fn hex_length(mut length: usize, digits: &mut [u8; 2 * size_of::<usize>()]) -> &str {
    let mut start = digits.len();
    loop {
        start -= 1;
        digits[start] = HEX[length & 15];
        length >>= 4;
        if length == 0 {
            return std::str::from_utf8(&digits[start..]).expect("hexadecimal digits are ASCII");
        }
    }
}

fn append_header(length: usize, buffer: &mut impl Buffer) -> Result<(), MemoryError> {
    buffer.append(PREFIX)?;
    buffer.append(hex_length(length, &mut [0; 2 * size_of::<usize>()]))?;
    buffer.append(".")
}

async fn join(
    graph: &str,
    name: &str,
    hex: bool,
    buffer: &mut impl Buffer,
    work: &mut impl Work,
) -> StorageBackendResult<()> {
    if hex {
        append_header(graph.len(), buffer)?;
    }
    component(graph, hex, buffer, work).await?;
    buffer.append(if hex { "." } else { "::" })?;
    component(name, hex, buffer, work).await
}

#[must_use]
pub fn key(graph: &str, name: &str) -> String {
    let mut key = String::new();
    let hex = inline(ambiguous(graph, name, &mut ())).expect("inline name scanning");
    inline(join(graph, name, hex, &mut key, &mut ())).expect("infallible string formatting");
    key
}

pub(super) async fn key_from(
    graph: &str,
    name: &str,
    input: &mut AsyncImageInput<'_>,
) -> StorageBackendResult<Budgeted<String>> {
    let hex = ambiguous(graph, name, input).await?;
    let length = graph
        .len()
        .checked_add(name.len())
        .ok_or(MemoryError::SizeOverflow)?;
    let length = if hex {
        let digits = hex_length(graph.len(), &mut [0; 2 * size_of::<usize>()]).len();
        length
            .checked_mul(2)
            .and_then(|n| n.checked_add(PREFIX.len() + digits + 2))
    } else {
        length.checked_add(2)
    }
    .ok_or(MemoryError::SizeOverflow)?;
    let mut buf = BudgetedString::new(input.memory);
    buf.reserve(length)?;
    join(graph, name, hex, &mut buf, input).await?;
    let (value, lease) = buf.into_parts();
    Ok(Budgeted::new(value, lease))
}

/// Prefixes for a scoped range scan. The encoded range can also contain literal graph names whose keys have a `::` separator; skip those when visiting the encoded range.
#[must_use]
pub fn prefixes(graph: &str) -> (Option<String>, String) {
    let raw = (!graph.ends_with(':') && !graph.contains("::")).then(|| format!("{graph}::"));
    let mut encoded = String::new();
    append_header(graph.len(), &mut encoded).expect("infallible string formatting");
    inline(component(graph, true, &mut encoded, &mut ())).expect("infallible string formatting");
    encoded.push('.');
    (raw, encoded)
}

pub(super) fn encoded_for(key: &str, graph: &str) -> bool {
    key.strip_prefix(PREFIX)
        .and_then(encoded_fields)
        .is_some_and(|(encoded, _)| {
            encoded.len() / 2 == graph.len()
                && encoded
                    .as_bytes()
                    .chunks_exact(2)
                    .map(hex_byte)
                    .eq(graph.bytes().map(Some))
                && !key.contains("::")
        })
}

fn hex_byte(digits: &[u8]) -> Option<u8> {
    let digit = |byte: u8| match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    };
    Some((digit(*digits.first()?)? << 4) | digit(*digits.get(1)?)?)
}

async fn text(
    hex: &str,
    memory: &MemoryBudget,
    work: &mut impl Work,
) -> StorageBackendResult<Option<Budgeted<String>>> {
    if !hex.len().is_multiple_of(2) {
        return Ok(None);
    }
    let mut value = BudgetedString::new(memory);
    value.reserve(hex.len() / 2)?;
    let bytes = hex.as_bytes();
    let mut at = 0;
    let mut next = 0;
    while at < bytes.len() {
        if at >= next {
            work.checkpoint().await?;
            next = at.saturating_add(WINDOW);
        }
        let Some(first) = hex_byte(&bytes[at..]) else {
            return Ok(None);
        };
        let width = match first {
            0..=127 => 1,
            0xc2..=0xdf => 2,
            0xe0..=0xef => 3,
            0xf0..=0xf4 => 4,
            _ => return Ok(None),
        };
        let mut scalar = [0; 4];
        for cell in &mut scalar[..width] {
            let Some(b) = hex_byte(&bytes[at..]) else {
                return Ok(None);
            };
            *cell = b;
            at += 2;
        }
        let Ok(s) = std::str::from_utf8(&scalar[..width]) else {
            return Ok(None);
        };
        value.push_str(s)?;
    }
    let (value, lease) = value.into_parts();
    Ok(Some(Budgeted::new(value, lease)))
}

fn encoded_fields(raw: &str) -> Option<(&str, &str)> {
    let header_limit = raw.len().min(2 * size_of::<usize>() + 1);
    let dot = raw.as_bytes()[..header_limit]
        .iter()
        .position(|byte| *byte == b'.')?;
    let count = &raw[..dot];
    if count.is_empty()
        || (count.len() > 1 && count.starts_with('0'))
        || !count
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return None;
    }
    let length = usize::from_str_radix(count, 16).ok()?.checked_mul(2)?;
    let (graph, rest) = raw[dot + 1..].split_at_checked(length)?;
    Some((graph, rest.strip_prefix('.')?))
}

pub(super) async fn decode(
    key: &str,
    memory: &MemoryBudget,
    work: &mut impl Work,
) -> StorageBackendResult<Option<(Budgeted<String>, Budgeted<String>)>> {
    let Some(raw) = key.strip_prefix(PREFIX) else {
        return Ok(None);
    };
    // Literal historical addresses can themselves start with the version prefix.
    for candidate in (Separators {
        raw: key,
        offset: 0,
    }) {
        work.checkpoint().await?;
        if candidate.is_some() {
            return Ok(None);
        }
    }
    let Some((graph, name)) = encoded_fields(raw) else {
        return Ok(None);
    };
    let Some(graph) = text(graph, memory, work).await? else {
        return Ok(None);
    };
    let Some(name) = text(name, memory, work).await? else {
        return Ok(None);
    };
    Ok(ambiguous(&graph, &name, work)
        .await?
        .then_some((graph, name)))
}
