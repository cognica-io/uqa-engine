//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Strict neutral-model input, retaining entry order and absent metadata.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs::File;
use std::io::Read;
use std::path::Path;

use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::nori::dictionary::provenance::Provenance;
use crate::nori::dictionary::provenance::FILES;
use crate::nori::dictionary::tables::{Characters, Matrix, CLASSES};
use crate::nori::error::{check_limit, invalid};
use crate::nori::io::{vector, Reader};
use crate::nori::lexicon::{Builder, Lexicon};
use crate::nori::morphology::{Morpheme, Morphology, WordEntry, ABSENT};
use crate::nori::unicode::{Properties, UnicodeRange, UnicodeTable, CODE_POINTS};
use crate::nori::{DictionaryLimits, DictionaryResult, POSTag, POSType, SurfaceWords};

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
}

pub(super) fn read_file(path: &Path, limit: usize) -> DictionaryResult<Vec<u8>> {
    let file = File::open(path)?;
    let length = usize::try_from(file.metadata()?.len())
        .map_err(|_| invalid("input file", "length exceeds address space"))?;
    check_limit("input bytes", length, limit)?;
    let mut bytes = vector(length)?;
    let maximum = u64::try_from(limit)
        .ok()
        .and_then(|limit| limit.checked_add(1))
        .ok_or_else(|| invalid("input file", "read limit overflow"))?;
    file.take(maximum).read_to_end(&mut bytes)?;
    check_limit("input bytes", bytes.len(), limit)?;
    Ok(bytes)
}

fn header<'a>(bytes: &'a [u8], magic: &[u8]) -> DictionaryResult<Reader<'a>> {
    let mut reader = Reader::new(bytes, "neutral model", true);
    if reader.take(8)? != magic {
        return Err(reader.invalid("invalid neutral file magic"));
    }
    Ok(reader)
}

fn text(reader: &mut Reader<'_>, limits: DictionaryLimits) -> DictionaryResult<Option<String>> {
    let count = reader.i32()?;
    if count == -1 {
        return Ok(None);
    }
    let count = usize::try_from(count).map_err(|_| reader.invalid("negative string length"))?;
    check_limit(
        "UTF-16 units per dictionary string",
        count,
        limits.max_text_utf16,
    )?;
    if count > reader.remaining() / 2 {
        return Err(reader.invalid("truncated UTF-16 string"));
    }
    let mut units = vector(count)?;
    for _ in 0..count {
        units.push(reader.u16()?);
    }
    Ok(Some(String::from_utf16(&units)?))
}

fn required_text(reader: &mut Reader<'_>, limits: DictionaryLimits) -> DictionaryResult<String> {
    text(reader, limits)?.ok_or_else(|| reader.invalid("required string is absent"))
}

fn count32(count: usize) -> DictionaryResult<u32> {
    u32::try_from(count).map_err(|_| invalid("neutral model", "count exceeds u32"))
}

struct Metadata {
    value: Morphology,
    interned: HashMap<String, u32>,
    limits: DictionaryLimits,
}

impl Metadata {
    fn string(&mut self, text: String) -> DictionaryResult<u32> {
        if let Some(id) = self.interned.get(&text) {
            return Ok(*id);
        }
        check_limit(
            "dictionary strings",
            self.value.strings.len() + 1,
            self.limits.max_strings,
        )?;
        let id = count32(self.value.strings.len())?;
        self.interned.try_reserve(1)?;
        self.value.strings.try_reserve(1)?;
        self.interned.insert(text.clone(), id);
        self.value.strings.push(text);
        Ok(id)
    }

    fn tag(reader: &mut Reader<'_>) -> DictionaryResult<POSTag> {
        let ordinal =
            u8::try_from(reader.u32()?).map_err(|_| reader.invalid("POS ordinal exceeds u8"))?;
        POSTag::from_ordinal(ordinal)
    }

    fn word(&mut self, reader: &mut Reader<'_>) -> DictionaryResult<WordEntry> {
        let original_id = reader.u32()?;
        let left =
            u16::try_from(reader.u32()?).map_err(|_| reader.invalid("left context exceeds u16"))?;
        let right = u16::try_from(reader.u32()?)
            .map_err(|_| reader.invalid("right context exceeds u16"))?;
        let cost =
            i16::try_from(reader.i32()?).map_err(|_| reader.invalid("word cost exceeds i16"))?;
        let ordinal =
            u8::try_from(reader.u32()?).map_err(|_| reader.invalid("POS type exceeds u8"))?;
        let pos_type = POSType::from_ordinal(ordinal)?;
        let left_pos = Self::tag(reader)?;
        let right_pos = Self::tag(reader)?;
        let reading = match text(reader, self.limits)? {
            None => ABSENT,
            Some(text) => self.string(text)?,
        };
        let count = reader.i32()?;
        let (morphemes, morpheme_count) = if count == -1 {
            (ABSENT, 0)
        } else {
            let count =
                usize::try_from(count).map_err(|_| reader.invalid("negative morpheme count"))?;
            if count > reader.remaining() / 8 {
                return Err(reader.invalid("morpheme count exceeds input"));
            }
            let start = count32(self.value.morphemes.len())?;
            self.value.morphemes.try_reserve(count)?;
            for _ in 0..count {
                let pos = Self::tag(reader)?;
                let surface = required_text(reader, self.limits)?;
                let surface = self.string(surface)?;
                self.value.morphemes.push(Morpheme { surface, pos });
            }
            (start, count32(count)?)
        };
        Ok(WordEntry {
            original_id,
            left,
            right,
            cost,
            pos_type,
            left_pos,
            right_pos,
            reading,
            morphemes,
            morpheme_count,
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
    let listed = manifest["files"]
        .as_array()
        .ok_or_else(|| invalid("neutral model", "missing file inventory"))?;
    let expected: BTreeSet<_> = FILES.iter().copied().collect();
    let mut names = BTreeSet::new();
    let mut inputs = BTreeMap::new();
    let mut total = 0_usize;
    for file in listed {
        let name = file["path"]
            .as_str()
            .ok_or_else(|| invalid("neutral model", "invalid file path"))?;
        if !expected.contains(name) || !names.insert(name) {
            return Err(invalid(
                "neutral model",
                "duplicate or unexpected input file",
            ));
        }
        let bytes = read_file(&directory.join(name), limits.max_decoded_bytes)?;
        total = total
            .checked_add(bytes.len())
            .ok_or_else(|| invalid("neutral model", "size overflow"))?;
        check_limit("neutral model bytes", total, limits.max_decoded_bytes)?;
        let hash = format!("{:x}", Sha256::digest(&bytes));
        if file["bytes"].as_u64() != Some(bytes.len() as u64) || file["sha256"] != hash {
            return Err(invalid(
                "neutral model",
                "input file size or checksum differs",
            ));
        }
        inputs.insert(name.to_owned(), bytes);
    }
    if names != expected {
        return Err(invalid("neutral model", "incomplete input inventory"));
    }
    let actual: BTreeSet<_> = std::fs::read_dir(directory)?
        .map(|entry| entry.map(|entry| entry.file_name()))
        .collect::<Result<_, _>>()?;
    let mut allowed: BTreeSet<_> = FILES.iter().map(std::ffi::OsString::from).collect();
    allowed.insert("model_manifest.json".into());
    if actual != allowed {
        return Err(invalid(
            "neutral model",
            "unexpected files in input directory",
        ));
    }

    Ok((manifest, inputs))
}

pub(super) fn read(directory: &Path, limits: DictionaryLimits) -> DictionaryResult<Model> {
    let (manifest, inputs) = read_inputs(directory, limits)?;
    let provenance = Provenance::from_value(manifest.clone())?;
    let mut reader = header(&inputs["connection_costs.bin"], b"UQANCCS1")?;
    let matrix = Matrix::decode(&mut reader)?;
    reader.finish()?;
    let mut metadata = Metadata {
        value: Morphology {
            strings: Vec::new(),
            morphemes: Vec::new(),
        },
        interned: HashMap::new(),
        limits,
    };
    let mut reader = header(&inputs["lexicon.bin"], b"UQANLEX1")?;
    let surface_count = reader.count(12)?;
    let known = reader.u32()?;
    if known as usize > reader.remaining() / 36 {
        return Err(reader.invalid("word count exceeds input"));
    }
    let mut words = vector(known as usize)?;
    let mut surfaces = vector(surface_count)?;
    let mut builder = Builder::new();
    for _ in 0..surface_count {
        let source_id = reader.u32()?;
        let surface = required_text(&mut reader, limits)?;
        builder.insert(surface.encode_utf16().collect())?;
        let count = reader.count(36)?;
        let start = count32(words.len())?;
        if count > known as usize - words.len() {
            return Err(reader.invalid("surface entries exceed declared known words"));
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
        return Err(reader.invalid("system word count differs"));
    }
    reader.finish()?;
    let lexicon = builder.finish(limits.max_text_utf16)?;

    let mut reader = header(&inputs["unknown.bin"], b"UQANUNK1")?;
    if reader.u32()? as usize != CLASSES.len() {
        return Err(reader.invalid("unknown class count differs"));
    }
    let unknown_count = reader.count(36)?;
    words.try_reserve(unknown_count)?;
    let mut unknown = vector(CLASSES.len())?;
    for (index, name) in CLASSES.iter().enumerate() {
        if reader.u32()? as usize != index || required_text(&mut reader, limits)? != *name {
            return Err(reader.invalid("unknown classes are not in canonical order"));
        }
        let count = reader.count(36)?;
        let start = count32(words.len())?;
        if count > unknown_count - (words.len() - known as usize) {
            return Err(reader.invalid("class entries exceed declared unknown words"));
        }
        for _ in 0..count {
            words.push(metadata.word(&mut reader)?);
        }
        unknown.push(start..count32(words.len())?);
    }
    if words.len() - known as usize != unknown_count {
        return Err(reader.invalid("unknown word count differs"));
    }
    reader.finish()?;

    let mut reader = header(&inputs["characters.bin"], b"UQANCHR1")?;
    if reader.u32()? as usize != CLASSES.len() {
        return Err(reader.invalid("character class count differs"));
    }
    let flags = reader.take(CLASSES.len())?.to_vec();
    if reader.u32()? != 0x1_0000 {
        return Err(reader.invalid("incomplete character table"));
    }
    let mut values = vector(0x1_0000)?;
    for _ in 0..0x1_0000 {
        values.push(reader.array()?);
    }
    reader.finish()?;
    let characters = Characters {
        flags,
        words: unknown,
        values,
    };

    let unicode = read_unicode(&inputs["unicode.bin"], provenance.scripts.len())?;
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
    })
}

fn read_unicode(bytes: &[u8], script_count: usize) -> DictionaryResult<UnicodeTable> {
    let mut reader = header(bytes, b"UQANUNI1")?;
    if reader.u32()? != CODE_POINTS {
        return Err(reader.invalid("incomplete Unicode profile"));
    }
    let mut ranges: Vec<UnicodeRange> = Vec::new();
    for code_point in 0..CODE_POINTS {
        let category = reader.u8()?;
        let script = reader.u16()?;
        let flags = reader.u8()?;
        let lowercase = reader.u32()?;
        if lowercase >= CODE_POINTS {
            return Err(reader.invalid("invalid lowercase code point"));
        }
        let properties = Properties {
            category,
            script,
            flags,
            lowercase_delta: lowercase as i32 - code_point as i32,
        };
        if let Some(previous) = ranges
            .last_mut()
            .filter(|range| range.properties == properties)
        {
            previous.end = code_point + 1;
        } else {
            ranges.try_reserve(1)?;
            ranges.push(UnicodeRange {
                end: code_point + 1,
                properties,
            });
        }
    }
    reader.finish()?;
    let unicode = UnicodeTable { ranges };
    unicode.validate(script_count)?;
    Ok(unicode)
}

#[cfg(test)]
mod tests {
    use super::{read_file, read_inputs, Metadata};
    use crate::nori::dictionary::provenance::FILES;
    use crate::nori::io::Reader;
    use crate::nori::morphology::Morphology;
    use crate::nori::{DictionaryError, DictionaryLimits};
    use sha2::{Digest, Sha256};

    fn inputs() -> tempfile::TempDir {
        let directory = tempfile::tempdir().unwrap();
        let mut manifest = crate::nori::tests::fixtures::provenance();
        for (index, name) in FILES.iter().enumerate() {
            let bytes = [index as u8];
            std::fs::write(directory.path().join(name), bytes).unwrap();
            manifest["files"][index]["sha256"] = format!("{:x}", Sha256::digest(bytes)).into();
        }
        std::fs::write(
            directory.path().join("model_manifest.json"),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        directory
    }

    #[test]
    fn neutral_inputs_require_an_exact_inventory_and_verified_bytes() {
        let directory = inputs();
        read_inputs(directory.path(), DictionaryLimits::default()).unwrap();
        std::fs::write(directory.path().join("unexpected.bin"), []).unwrap();
        assert!(read_inputs(directory.path(), DictionaryLimits::default()).is_err());
        std::fs::remove_file(directory.path().join("unexpected.bin")).unwrap();
        std::fs::write(directory.path().join("lexicon.bin"), [99]).unwrap();
        assert!(read_inputs(directory.path(), DictionaryLimits::default()).is_err());
        std::fs::remove_file(directory.path().join("lexicon.bin")).unwrap();
        assert!(read_inputs(directory.path(), DictionaryLimits::default()).is_err());
    }

    #[test]
    fn offline_file_reads_enforce_the_configured_limit() {
        let directory = inputs();
        let error = read_file(&directory.path().join("model_manifest.json"), 1).unwrap_err();
        assert!(matches!(error, DictionaryError::Limit { .. }));
    }

    #[test]
    fn neutral_word_metadata_rejects_lossy_integer_and_utf16_conversions() {
        for (field, value) in [
            (0, 0),
            (1, 65536),
            (2, 65536),
            (3, 32768),
            (4, 256),
            (5, 48),
            (7, -2),
        ] {
            let mut fields = [0_i32, 0, 0, 0, 0, 0, 0, -1, -1];
            fields[field] = value;
            let mut bytes: Vec<_> = fields.into_iter().flat_map(i32::to_be_bytes).collect();
            if field == 0 {
                // Replace the absent reading with one high surrogate followed by an absent morpheme list.
                bytes.truncate(28);
                bytes.extend(1_i32.to_be_bytes());
                bytes.extend(0xd800_u16.to_be_bytes());
                bytes.extend((-1_i32).to_be_bytes());
            }
            let mut metadata = Metadata {
                value: Morphology {
                    strings: Vec::new(),
                    morphemes: Vec::new(),
                },
                interned: std::collections::HashMap::new(),
                limits: DictionaryLimits::default(),
            };
            assert!(metadata
                .word(&mut Reader::new(&bytes, "neutral test", true))
                .is_err());
        }
    }
}
