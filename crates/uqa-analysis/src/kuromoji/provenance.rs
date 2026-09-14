//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Canonical model provenance and pinned vocabulary validation.

use std::collections::BTreeSet;

use serde_json::Value;

use super::tables::CLASSES;
use super::KuromojiDictionary;
use crate::kuromoji::error::{check_limit, invalid};
use crate::kuromoji::DictionaryResult;

mod reference;

pub(super) const FILES: &[&str] = &[
    "lexicon.bin",
    "unknown.bin",
    "connection_costs.bin",
    "characters.bin",
    "unicode.bin",
    "analysis.bin",
];

#[derive(Debug)]
pub(super) struct Provenance {
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
        if json["format"] != "uqa-kuromoji-neutral"
            || json["format_version"] != 1
            || json["byte_order"] != "big"
        {
            return Err(invalid("provenance", "unsupported neutral model format"));
        }
        let model = &json["model"];
        let fields = strings(&model["morphology_fields"])?;
        if fields.iter().map(String::as_str).ne([
            "part_of_speech",
            "base_form",
            "reading",
            "pronunciation",
            "inflection_type",
            "inflection_form",
        ]) {
            return Err(invalid(
                "provenance",
                "Japanese morphology vocabulary differs",
            ));
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

    pub fn validate_counts(&self, dictionary: &KuromojiDictionary) -> DictionaryResult<()> {
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
            (
                "unicode_count",
                crate::morphology::unicode::CODE_POINTS as usize,
            ),
            ("character_count", dictionary.characters.values.len()),
            ("stop_word_count", dictionary.analysis.stop_words.len()),
            ("stop_tag_count", dictionary.analysis.stop_tags.len()),
            (
                "completion_mapping_count",
                dictionary.analysis.completion.len(),
            ),
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
    crate::morphology::manifest::strings(value).map_err(Into::into)
}

pub(super) fn canonical(value: &Value) -> DictionaryResult<Vec<u8>> {
    crate::morphology::manifest::canonical(value).map_err(Into::into)
}
