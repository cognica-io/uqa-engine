//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Canonical metadata and structural identity validation for dictionary provenance.

use super::error::invalid;
use super::work::Work;
use super::DictionaryResult;
use crate::json::fragment;
use serde_json::Value;
use std::collections::BTreeSet;

mod canonical;
mod comparison;
mod input;

pub(crate) use canonical::canonical_with;
pub(crate) use comparison::{bytes_equal, equal_with};
pub(crate) use input::parse;

pub(crate) async fn strings_with(
    value: &Value,
    work: &mut impl Work,
) -> DictionaryResult<Vec<String>> {
    let values = value
        .as_array()
        .ok_or_else(|| invalid("provenance", "missing vocabulary array"))?;
    let mut strings = super::io::vector(values.len())?;
    for (index, value) in values.iter().enumerate() {
        work.records(index).await?;
        let text = value
            .as_str()
            .ok_or_else(|| invalid("provenance", "vocabulary entry is not a string"))?;
        let mut owned = String::new();
        owned.try_reserve_exact(text.len())?;
        let mut position = 0;
        while position < text.len() {
            work.checkpoint().await?;
            let part = fragment(text, position);
            owned.push_str(part);
            position += part.len();
        }
        strings.push(owned);
    }
    Ok(strings)
}

#[cfg(any(test, feature = "nori-tools", feature = "kuromoji-tools"))]
pub(crate) fn canonical(value: &Value) -> DictionaryResult<Vec<u8>> {
    super::work::inline(canonical_with(value, &mut ()))
}

pub(crate) fn text(value: &Value) -> DictionaryResult<&str> {
    value
        .as_str()
        .filter(|text| !text.is_empty())
        .ok_or_else(|| invalid("provenance", "missing nonempty identity string"))
}

pub(crate) fn hex(value: &Value, length: usize) -> DictionaryResult<()> {
    let value = text(value)?;
    hex_text(value, length)
}

fn hex_text(value: &str, length: usize) -> DictionaryResult<()> {
    if value.is_empty() {
        return Err(invalid("provenance", "missing nonempty identity string"));
    }
    if value.len() != length
        || !value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(invalid(
            "provenance",
            "invalid hexadecimal digest or commit",
        ));
    }
    Ok(())
}

pub(crate) async fn inventory_with(
    value: &Value,
    key: &str,
    expected: &[&str],
    work: &mut impl Work,
) -> DictionaryResult<()> {
    let entries = value
        .as_array()
        .ok_or_else(|| invalid("provenance", "missing inventory"))?;
    let mut names = BTreeSet::new();
    for (index, entry) in entries.iter().enumerate() {
        work.records(index).await?;
        let name = text(&entry[key])?;
        if !names.insert(name) || entry["bytes"].as_u64().is_none_or(|bytes| bytes == 0) {
            return Err(invalid(
                "provenance",
                "duplicate identity or invalid byte count",
            ));
        }
        hex(&entry["sha256"], 64)?;
    }
    let mut expected_names = BTreeSet::new();
    for (index, name) in expected.iter().enumerate() {
        work.records(index).await?;
        expected_names.insert(*name);
    }
    let mut same = names.len() == expected_names.len();
    if same {
        for (index, (actual, expected)) in names.iter().zip(expected_names).enumerate() {
            work.records(index).await?;
            if !bytes_equal(actual.as_bytes(), expected.as_bytes(), work).await? {
                same = false;
                break;
            }
        }
    }
    if !same {
        return Err(invalid(
            "provenance",
            "inventory differs from expected resources",
        ));
    }
    Ok(())
}

pub(crate) async fn scripts_with(
    value: &Value,
    work: &mut impl Work,
) -> DictionaryResult<Vec<String>> {
    let scripts = strings_with(value, work).await?;
    let mut unique = BTreeSet::new();
    for (index, name) in scripts.iter().enumerate() {
        work.records(index).await?;
        unique.insert(name.as_str());
    }
    if scripts.len() > 0x1_0000
        || unique.len() != scripts.len()
        || !unique.contains("COMMON")
        || !unique.contains("INHERITED")
        || !unique.contains("UNKNOWN")
    {
        return Err(invalid("provenance", "invalid Unicode script vocabulary"));
    }
    for (index, name) in scripts.iter().enumerate() {
        work.records(index).await?;
        if name.is_empty() {
            return Err(invalid("provenance", "invalid Unicode script vocabulary"));
        }
        for bytes in name.as_bytes().chunks(4096) {
            work.checkpoint().await?;
            if !bytes.iter().all(|b| b.is_ascii_uppercase() || *b == b'_') {
                return Err(invalid("provenance", "invalid Unicode script vocabulary"));
            }
        }
    }
    Ok(scripts)
}

pub(crate) async fn pinned_digest_with(
    value: &Value,
    work: &mut impl Work,
) -> DictionaryResult<()> {
    let image = text(value)?;
    let bytes = image.as_bytes();
    let marker = b"@sha256:";
    let mut start = 0;
    while start < bytes.len() {
        work.checkpoint().await?;
        let end = start
            .saturating_add(4096 + marker.len() - 1)
            .min(bytes.len());
        if let Some(offset) = bytes[start..end]
            .windows(marker.len())
            .position(|part| part == marker)
        {
            if start + offset == 0 {
                break;
            }
            return hex_text(&image[start + offset + marker.len()..], 64);
        }
        start = start.saturating_add(4096);
    }
    Err(invalid("provenance", "Docker image is not digest pinned"))
}

#[cfg(test)]
pub(crate) mod tests;
