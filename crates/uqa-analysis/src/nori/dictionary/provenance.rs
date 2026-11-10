//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Canonical model provenance and pinned vocabulary validation.

use crate::morphology::{manifest, work::Work};
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
    pub async fn from_section(
        section: crate::morphology::frame::Section,
        maximum_bytes: usize,
        work: &mut impl Work,
    ) -> DictionaryResult<Self> {
        let mut reader = section.reader();
        let bytes = reader.take(reader.remaining())?;
        let value = Self::decode_with(bytes, maximum_bytes, work).await?;
        section.finish(reader, 1)?;
        Ok(value)
    }

    #[cfg(test)]
    pub fn decode(bytes: &[u8], maximum_bytes: usize) -> DictionaryResult<Self> {
        crate::morphology::work::inline(Self::decode_with(bytes, maximum_bytes, &mut ()))
    }

    pub async fn decode_with(
        bytes: &[u8],
        maximum_bytes: usize,
        work: &mut impl Work,
    ) -> DictionaryResult<Self> {
        check_limit("manifest bytes", bytes.len(), maximum_bytes)?;
        let json = manifest::parse(bytes, work).await?;
        let canonical = manifest::canonical_with(&json, work).await?;
        if !manifest::bytes_equal(&canonical, bytes, work).await? {
            return Err(invalid("provenance", "manifest is not canonical JSON"));
        }
        Self::from_value_with(json, work).await
    }

    #[cfg(any(test, feature = "nori-tools"))]
    pub fn from_value(json: Value) -> DictionaryResult<Self> {
        crate::morphology::work::inline(Self::from_value_with(json, &mut ()))
    }

    pub async fn from_value_with(json: Value, work: &mut impl Work) -> DictionaryResult<Self> {
        if json["format"] != "uqa-nori-neutral"
            || json["format_version"] != 1
            || json["byte_order"] != "big"
        {
            return Err(invalid("provenance", "unsupported neutral model format"));
        }
        let model = &json["model"];
        let types = manifest::strings_with(&model["pos_types"], work).await?;
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
            work.records(index).await?;
            let tag = POSTag::from_ordinal(index as u8)?;
            if actual["name"] != tag.name()
                || actual["code"].as_i64() != Some(i64::from(tag.code()))
            {
                return Err(invalid("provenance", "POS names or codes differ"));
            }
        }
        if manifest::strings_with(&model["character_classes"], work)
            .await?
            .iter()
            .map(String::as_str)
            .ne(CLASSES.iter().copied())
        {
            return Err(invalid("provenance", "character class vocabulary differs"));
        }
        let scripts = manifest::scripts_with(&model["unicode_scripts"], work).await?;
        reference::validate_with(&json, work).await?;
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
            (
                "unicode_count",
                crate::morphology::unicode::CODE_POINTS as usize,
            ),
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

#[cfg(any(test, feature = "nori-tools"))]
pub(in crate::nori) fn canonical(value: &Value) -> DictionaryResult<Vec<u8>> {
    crate::morphology::manifest::canonical(value).map_err(Into::into)
}
