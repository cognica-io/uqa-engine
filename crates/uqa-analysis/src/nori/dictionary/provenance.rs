//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Canonical model provenance and pinned vocabulary validation.

use std::collections::BTreeSet;

use serde_json::Value;

use super::tables::CLASSES;
use super::NoriDictionary;
use crate::nori::error::{check_limit, invalid};
use crate::nori::{DictionaryResult, POSTag, POSType};

mod reference;

pub(in crate::nori) const FILES: &[&str] = &[
    "lexicon.bin",
    "unknown.bin",
    "connection_costs.bin",
    "characters.bin",
    "unicode.bin",
];

#[derive(Debug)]
pub(in crate::nori) struct Provenance {
    pub json: Value,
    pub scripts: Vec<String>,
}

impl Provenance {
    pub fn decode(bytes: &[u8], maximum_bytes: usize) -> DictionaryResult<Self> {
        check_limit("manifest bytes", bytes.len(), maximum_bytes)?;
        let json: Value = serde_json::from_slice(bytes)?;
        if canonical(&json)? != bytes {
            return Err(invalid("provenance", "manifest is not canonical JSON"));
        }
        Self::from_value(json)
    }

    pub fn from_value(json: Value) -> DictionaryResult<Self> {
        if json["format"] != "uqa-nori-neutral"
            || json["format_version"] != 1
            || json["byte_order"] != "big"
        {
            return Err(invalid("provenance", "unsupported neutral model format"));
        }
        let model = &json["model"];
        let types = strings(&model["pos_types"])?;
        let expected = [
            POSType::Morpheme,
            POSType::Compound,
            POSType::Inflect,
            POSType::Preanalysis,
        ];
        if types
            .iter()
            .map(String::as_str)
            .ne(expected.iter().map(|kind| kind.name()))
        {
            return Err(invalid("provenance", "POS type vocabulary differs"));
        }
        let tags = model["pos_tags"]
            .as_array()
            .ok_or_else(|| invalid("provenance", "missing POS tags"))?;
        if tags.len() != POSTag::NAMES.len() {
            return Err(invalid("provenance", "POS tag count differs"));
        }
        for (index, actual) in tags.iter().enumerate() {
            let tag = POSTag::from_ordinal(index as u8)?;
            if actual["name"] != tag.name()
                || actual["code"].as_i64() != Some(i64::from(tag.code()))
            {
                return Err(invalid("provenance", "POS names or codes differ"));
            }
        }
        if strings(&model["character_classes"])?
            .iter()
            .map(String::as_str)
            .ne(CLASSES.iter().copied())
        {
            return Err(invalid("provenance", "character class vocabulary differs"));
        }
        let scripts = strings(&model["unicode_scripts"])?;
        let unique: BTreeSet<_> = scripts.iter().map(String::as_str).collect();
        if scripts.len() > 0x1_0000
            || unique.len() != scripts.len()
            || !unique.contains("COMMON")
            || !unique.contains("INHERITED")
            || !unique.contains("UNKNOWN")
            || scripts.iter().any(|name| {
                name.is_empty() || !name.bytes().all(|b| b.is_ascii_uppercase() || b == b'_')
            })
        {
            return Err(invalid("provenance", "invalid Unicode script vocabulary"));
        }
        reference::validate(&json)?;
        Ok(Self { json, scripts })
    }

    pub fn validate_counts(&self, dictionary: &NoriDictionary) -> DictionaryResult<()> {
        let counts = [
            ("surface_count", dictionary.surfaces.len()),
            ("word_count", dictionary.known_words as usize),
            (
                "unknown_word_count",
                dictionary.words.len() - dictionary.known_words as usize,
            ),
            ("unknown_class_count", dictionary.characters.words.len()),
            ("matrix_forward", dictionary.matrix.forward),
            ("matrix_backward", dictionary.matrix.backward),
            ("unicode_count", crate::nori::unicode::CODE_POINTS as usize),
            ("character_count", dictionary.characters.values.len()),
        ];
        for (name, expected) in counts {
            if self.json["model"][name].as_u64() != Some(expected as u64) {
                return Err(invalid(
                    "provenance",
                    "model counts differ from decoded tables",
                ));
            }
        }
        Ok(())
    }
}

fn strings(value: &Value) -> DictionaryResult<Vec<String>> {
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

pub(in crate::nori) fn canonical(value: &Value) -> DictionaryResult<Vec<u8>> {
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
