//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Canonical metadata and structural identity validation for dictionary provenance.

use super::error::invalid;
use super::DictionaryResult;
use serde_json::Value;
use std::collections::BTreeSet;

pub(crate) fn strings(value: &Value) -> DictionaryResult<Vec<String>> {
    value
        .as_array()
        .ok_or_else(|| invalid("provenance", "missing vocabulary array"))?
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| invalid("provenance", "vocabulary entry is not a string"))
        })
        .collect()
}

pub(crate) fn canonical(value: &Value) -> DictionaryResult<Vec<u8>> {
    fn write(value: &Value, output: &mut Vec<u8>) -> DictionaryResult<()> {
        match value {
            Value::Array(values) => {
                output.push(b'[');
                for (index, value) in values.iter().enumerate() {
                    if index != 0 {
                        output.push(b',');
                    }
                    write(value, output)?;
                }
                output.push(b']');
            }
            Value::Object(values) => {
                output.push(b'{');
                let mut keys: Vec<_> = values.keys().collect();
                keys.sort_unstable();
                for (index, key) in keys.into_iter().enumerate() {
                    if index != 0 {
                        output.push(b',');
                    }
                    serde_json::to_writer(&mut *output, key)?;
                    output.push(b':');
                    write(&values[key], output)?;
                }
                output.push(b'}');
            }
            _ => serde_json::to_writer(&mut *output, value)?,
        }
        Ok(())
    }
    let mut output = Vec::new();
    write(value, &mut output)?;
    Ok(output)
}

pub(crate) fn text(value: &Value) -> DictionaryResult<&str> {
    value
        .as_str()
        .filter(|text| !text.is_empty())
        .ok_or_else(|| invalid("provenance", "missing nonempty identity string"))
}

pub(crate) fn hex(value: &Value, length: usize) -> DictionaryResult<()> {
    let value = text(value)?;
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

pub(crate) fn inventory(value: &Value, key: &str, expected: &[&str]) -> DictionaryResult<()> {
    let entries = value
        .as_array()
        .ok_or_else(|| invalid("provenance", "missing inventory"))?;
    let mut names = BTreeSet::new();
    for entry in entries {
        let name = text(&entry[key])?;
        if !names.insert(name) || entry["bytes"].as_u64().is_none_or(|bytes| bytes == 0) {
            return Err(invalid(
                "provenance",
                "duplicate identity or invalid byte count",
            ));
        }
        hex(&entry["sha256"], 64)?;
    }
    if names != expected.iter().copied().collect() {
        return Err(invalid(
            "provenance",
            "inventory differs from expected resources",
        ));
    }
    Ok(())
}
