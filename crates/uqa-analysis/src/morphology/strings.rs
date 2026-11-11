//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bounded UTF-8 string pools shared by language-specific morphology records.

use super::error::check_limit;
use super::io::{vector, Reader};
use super::work::Work;
use super::DictionaryResult;

pub(crate) async fn decode_with(
    reader: &mut Reader<'_>,
    maximum_strings: usize,
    maximum_text: usize,
    work: &mut impl Work,
) -> DictionaryResult<Vec<String>> {
    let count = reader.count(4)?;
    check_limit("dictionary strings", count, maximum_strings)?;
    let mut strings = vector(count)?;
    for index in 0..count {
        work.records(index).await?;
        strings.push(text_with(reader, maximum_text, work).await?);
    }
    Ok(strings)
}

pub(crate) async fn text_with(
    reader: &mut Reader<'_>,
    maximum_text: usize,
    work: &mut impl Work,
) -> DictionaryResult<String> {
    let bytes = reader.text_bytes()?;
    let mut position = 0;
    let mut units = 0;
    while position < bytes.len() {
        work.checkpoint().await?;
        let text = fragment(bytes, position, work)?;
        units += text.encode_utf16().count();
        position += text.len();
    }
    check_limit("UTF-16 units per dictionary string", units, maximum_text)?;
    let mut owned = String::new();
    owned.try_reserve_exact(bytes.len())?;
    position = 0;
    while position < bytes.len() {
        work.checkpoint().await?;
        let text = fragment(bytes, position, work)?;
        owned.push_str(text);
        position += text.len();
    }
    Ok(owned)
}

fn fragment<'a>(bytes: &'a [u8], position: usize, work: &impl Work) -> DictionaryResult<&'a str> {
    let end = position.saturating_add(4096).min(bytes.len());
    let fragment = &bytes[position..end];
    match std::str::from_utf8(fragment) {
        Ok(text) => Ok(text),
        Err(error) if error.error_len().is_none() && end < bytes.len() => {
            // The incomplete scalar stays in the original bytes for the next bounded fragment.
            Ok(std::str::from_utf8(&fragment[..error.valid_up_to()])?)
        }
        Err(error) => Err(work.utf8_error(bytes, position, error)),
    }
}

#[cfg(test)]
mod tests;

#[cfg(any(test, feature = "nori-tools", feature = "kuromoji-tools"))]
pub(crate) fn encode(strings: &[String], output: &mut super::io::Writer) -> DictionaryResult<()> {
    output.count(strings.len())?;
    for string in strings {
        output.text(string)?;
    }
    Ok(())
}

#[cfg(any(feature = "nori-tools", feature = "kuromoji-tools"))]
pub(crate) fn intern(
    strings: &mut Vec<String>,
    interned: &mut std::collections::HashMap<String, u32>,
    text: String,
    maximum: usize,
) -> DictionaryResult<u32> {
    if let Some(id) = interned.get(&text) {
        return Ok(*id);
    }
    check_limit("dictionary strings", strings.len() + 1, maximum)?;
    let id = u32::try_from(strings.len())
        .map_err(|_| super::error::invalid("neutral model", "count exceeds u32"))?;
    interned.try_reserve(1)?;
    strings.try_reserve(1)?;
    interned.insert(text.clone(), id);
    strings.push(text);
    Ok(id)
}
