//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Strict neutral-model input, retaining entry order and absent metadata.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use serde_json::Value;

use crate::kuromoji::error::check_limit;
use crate::kuromoji::morphology::{Morphology, WordEntry, ABSENT};
use crate::kuromoji::provenance::Provenance;
use crate::kuromoji::provenance::FILES;
use crate::kuromoji::tables::{Characters, CLASSES};
use crate::kuromoji::{DictionaryLimits, DictionaryResult, SurfaceWords};
use crate::morphology::io::{vector, Reader};
use crate::morphology::lexicon::{Builder, Lexicon};
use crate::morphology::matrix::Matrix;
use crate::morphology::unicode::UnicodeTable;

pub(super) struct Model {
    pub manifest: Value,
    pub lexicon: Lexicon,
    pub surfaces: Vec<SurfaceWords>,
    pub known: u32,
    pub words: Vec<WordEntry>,
    pub morphology: Morphology,
    pub matrix: Matrix,
    pub characters: Characters,
    pub unicode: UnicodeTable,
    pub analysis: crate::kuromoji::analysis::AnalysisData,
}

pub(super) fn read_file(path: &Path, limit: usize) -> DictionaryResult<Vec<u8>> {
    crate::morphology::neutral::read_file(path, limit).map_err(Into::into)
}

use crate::morphology::neutral::{count32, header, required_text, text};

struct Metadata {
    value: Morphology,
    interned: HashMap<String, u32>,
    limits: DictionaryLimits,
}

impl Metadata {
    fn string(&mut self, text: String) -> DictionaryResult<u32> {
        crate::morphology::strings::intern(
            &mut self.value.strings,
            &mut self.interned,
            text,
            self.limits.max_strings,
        )
        .map_err(Into::into)
    }

    fn word(&mut self, reader: &mut Reader<'_>) -> DictionaryResult<WordEntry> {
        let original_id = reader.u32()?;
        let left =
            u16::try_from(reader.u32()?).map_err(|_| reader.invalid("left context exceeds u16"))?;
        let right = u16::try_from(reader.u32()?)
            .map_err(|_| reader.invalid("right context exceeds u16"))?;
        let cost =
            i16::try_from(reader.i32()?).map_err(|_| reader.invalid("word cost exceeds i16"))?;
        let mut attributes = [ABSENT; 6];
        for attribute in &mut attributes {
            if let Some(text) = text(reader, self.limits)? {
                *attribute = self.string(text)?;
            }
        }
        Ok(WordEntry {
            original_id,
            left,
            right,
            cost,
            attributes,
        })
    }
}

fn read_inputs(
    directory: &Path,
    limits: DictionaryLimits,
) -> DictionaryResult<(Value, BTreeMap<String, Vec<u8>>)> {
    let manifest_bytes = read_file(
        &directory.join("model_manifest.json"),
        limits.max_manifest_bytes,
    )?;
    let manifest: Value = serde_json::from_slice(&manifest_bytes)?;
    Provenance::from_value(manifest.clone())?;
    let inputs = crate::morphology::neutral::read_inputs(directory, &manifest, FILES, limits)?;

    Ok((manifest, inputs))
}

pub(super) fn read(directory: &Path, limits: DictionaryLimits) -> DictionaryResult<Model> {
    let (manifest, inputs) = read_inputs(directory, limits)?;
    let provenance = Provenance::from_value(manifest.clone())?;
    let mut reader = header(&inputs["connection_costs.bin"], b"UQAJCCS1")?;
    let matrix = Matrix::decode(&mut reader)?;
    reader.finish()?;
    let mut metadata = Metadata {
        value: Morphology {
            strings: Vec::new(),
        },
        interned: HashMap::new(),
        limits,
    };
    let mut reader = header(&inputs["lexicon.bin"], b"UQAJLEX1")?;
    let surface_count = reader.count(12)?;
    let known = reader.u32()?;
    if known as usize > reader.remaining() / 40 {
        return Err(reader.invalid("word count exceeds input").into());
    }
    let mut words = vector(known as usize)?;
    let mut surfaces = vector(surface_count)?;
    let mut builder = Builder::new();
    for _ in 0..surface_count {
        let source_id = reader.u32()?;
        let surface = required_text(&mut reader, limits)?;
        builder.insert(surface.encode_utf16().collect())?;
        let count = reader.count(40)?;
        let start = count32(words.len())?;
        if count > known as usize - words.len() {
            return Err(reader
                .invalid("surface entries exceed declared known words")
                .into());
        }
        for _ in 0..count {
            words.push(metadata.word(&mut reader)?);
        }
        surfaces.push(SurfaceWords {
            source_id,
            word_ids: start..count32(words.len())?,
        });
    }
    if words.len() != known as usize {
        return Err(reader.invalid("system word count differs").into());
    }
    reader.finish()?;
    let lexicon = builder.finish(limits.max_text_utf16)?;

    let mut reader = header(&inputs["unknown.bin"], b"UQAJUNK1")?;
    if reader.u32()? as usize != CLASSES.len() {
        return Err(reader.invalid("unknown class count differs").into());
    }
    let unknown_count = reader.count(40)?;
    words.try_reserve(unknown_count)?;
    let mut unknown = vector(CLASSES.len())?;
    for (index, name) in CLASSES.iter().enumerate() {
        if reader.u32()? as usize != index || required_text(&mut reader, limits)? != *name {
            return Err(reader
                .invalid("unknown classes are not in canonical order")
                .into());
        }
        let count = reader.count(40)?;
        let start = count32(words.len())?;
        if count > unknown_count - (words.len() - known as usize) {
            return Err(reader
                .invalid("class entries exceed declared unknown words")
                .into());
        }
        for _ in 0..count {
            words.push(metadata.word(&mut reader)?);
        }
        unknown.push(start..count32(words.len())?);
    }
    if words.len() - known as usize != unknown_count {
        return Err(reader.invalid("unknown word count differs").into());
    }
    reader.finish()?;

    let characters = read_characters(&inputs["characters.bin"], unknown)?;

    let unicode = read_unicode(&inputs["unicode.bin"], provenance.scripts.len())?;
    let mut analysis_limits = limits;
    analysis_limits.max_strings -= metadata.value.strings.len();
    let analysis = read_analysis(&inputs["analysis.bin"], analysis_limits)?;
    Ok(Model {
        manifest,
        lexicon,
        surfaces,
        known,
        words,
        morphology: metadata.value,
        matrix,
        characters,
        unicode,
        analysis,
    })
}

fn read_characters(bytes: &[u8], words: Vec<std::ops::Range<u32>>) -> DictionaryResult<Characters> {
    let mut reader = header(bytes, b"UQAJCHR1")?;
    if reader.u32()? as usize != CLASSES.len() {
        return Err(reader.invalid("character class count differs").into());
    }
    let flags = reader.take(CLASSES.len())?.to_vec();
    if reader.u32()? != 0x1_0000 {
        return Err(reader.invalid("incomplete character table").into());
    }
    let mut values = vector(0x1_0000)?;
    for _ in 0..0x1_0000 {
        values.push(reader.array()?);
    }
    reader.finish()?;
    Ok(Characters {
        flags,
        words,
        values,
    })
}

fn read_unicode(bytes: &[u8], script_count: usize) -> DictionaryResult<UnicodeTable> {
    crate::morphology::unicode::read_neutral(header(bytes, b"UQAJUNI1")?, script_count)
        .map_err(Into::into)
}

fn read_analysis(
    bytes: &[u8],
    limits: DictionaryLimits,
) -> DictionaryResult<crate::kuromoji::analysis::AnalysisData> {
    use crate::kuromoji::analysis::{AnalysisData, CompletionMapping};
    fn texts(
        reader: &mut Reader<'_>,
        limits: DictionaryLimits,
        remaining: &mut usize,
    ) -> DictionaryResult<Vec<String>> {
        let count = reader.count(4)?;
        check_limit("analysis strings", count, *remaining)?;
        *remaining -= count;
        let mut values = vector(count)?;
        for _ in 0..count {
            values.push(required_text(reader, limits)?);
        }
        Ok(values)
    }
    let mut reader = header(bytes, b"UQAJANA1")?;
    let mut remaining = limits.max_strings;
    let stop_words = texts(&mut reader, limits, &mut remaining)?;
    let stop_tags = texts(&mut reader, limits, &mut remaining)?;
    let count = reader.count(8)?;
    let minimum_strings = count
        .checked_mul(2)
        .ok_or_else(|| reader.invalid("string count overflow"))?;
    check_limit("analysis strings", minimum_strings, remaining)?;
    let mut completion = vector(count)?;
    for _ in 0..count {
        check_limit("analysis strings", 1, remaining)?;
        remaining -= 1;
        completion.push(CompletionMapping {
            key: required_text(&mut reader, limits)?,
            alternatives: texts(&mut reader, limits, &mut remaining)?,
        });
    }
    reader.finish()?;
    Ok(AnalysisData {
        stop_words,
        stop_tags,
        completion,
    })
}

#[cfg(test)]
mod tests {
    use super::read_analysis;
    use crate::kuromoji::{DictionaryError, DictionaryLimits};

    #[test]
    fn neutral_analysis_enforces_one_string_limit_across_all_resources() {
        // Four UTF-16 strings: stopword a, stop tag b, completion c -> d.
        let bytes = b"UQAJANA1\0\0\0\x01\0\0\0\x01\0a\0\0\0\x01\0\0\0\x01\0b\0\0\0\x01\0\0\0\x01\0c\0\0\0\x01\0\0\0\x01\0d";
        for max_strings in 0..4 {
            assert!(matches!(
                read_analysis(
                    bytes,
                    DictionaryLimits {
                        max_strings,
                        ..DictionaryLimits::default()
                    }
                ),
                Err(DictionaryError::Limit { .. })
            ));
        }
        let data = read_analysis(
            bytes,
            DictionaryLimits {
                max_strings: 4,
                ..DictionaryLimits::default()
            },
        )
        .unwrap();
        assert_eq!(data.stop_words, ["a"]);
        assert_eq!(data.stop_tags, ["b"]);
        assert_eq!(data.completion[0].key(), "c");
        assert_eq!(data.completion[0].alternatives(), ["d"]);
    }
}
