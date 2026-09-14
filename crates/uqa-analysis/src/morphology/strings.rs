//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bounded UTF-8 string pools shared by language-specific morphology records.

use super::error::check_limit;
use super::io::{vector, Reader};
use super::DictionaryResult;

pub(crate) fn decode(
    reader: &mut Reader<'_>,
    maximum_strings: usize,
    maximum_text: usize,
) -> DictionaryResult<Vec<String>> {
    let count = reader.count(4)?;
    check_limit("dictionary strings", count, maximum_strings)?;
    let mut strings = vector(count)?;
    for _ in 0..count {
        let text = reader.text()?;
        check_limit(
            "UTF-16 units per dictionary string",
            text.encode_utf16().count(),
            maximum_text,
        )?;
        let mut owned = String::new();
        owned.try_reserve_exact(text.len())?;
        owned.push_str(text);
        strings.push(owned);
    }
    Ok(strings)
}

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
