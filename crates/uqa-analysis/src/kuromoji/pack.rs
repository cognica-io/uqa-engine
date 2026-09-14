//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Offline conversion of a verified neutral model into the runtime bundle.

use std::path::Path;

use super::frame::{self, Section};
use super::provenance::canonical;
use super::{DictionaryLimits, DictionaryResult, KuromojiDictionary};
use crate::morphology::io::Writer;

mod neutral;
mod verify;

pub use verify::verify_dictionary;

/// Read a bounded offline artifact and verify its complete neutral-model reconstruction.
pub fn verify_bundle_file(
    path: &Path,
    limits: DictionaryLimits,
) -> DictionaryResult<std::sync::Arc<KuromojiDictionary>> {
    let bytes = neutral::read_file(path, limits.max_encoded_bytes)?;
    let dictionary = KuromojiDictionary::from_bytes(&bytes, limits)?;
    verify_dictionary(&dictionary, limits)?;
    Ok(dictionary)
}

/// Build a deterministic bundle and verify every decoded value against the neutral input hashes.
pub fn pack_directory(directory: &Path, limits: DictionaryLimits) -> DictionaryResult<Vec<u8>> {
    let model = neutral::read(directory, limits)?;
    let mut sections = Vec::new();
    let mut write_section =
        |kind, records, encode: &mut dyn FnMut(&mut Writer) -> DictionaryResult<()>| {
            let mut output = Writer::default();
            encode(&mut output)?;
            sections.push(Section {
                kind,
                records,
                bytes: output.0,
            });
            Ok::<(), super::DictionaryError>(())
        };
    write_section(1, model.surfaces.len() as u64, &mut |output| {
        crate::morphology::surfaces::encode(&model.lexicon, &model.surfaces, output)
            .map_err(Into::into)
    })?;
    write_section(2, model.words.len() as u64, &mut |output| {
        super::morphology::encode_words(&model.words, model.known, output)
    })?;
    write_section(3, model.morphology.strings.len() as u64, &mut |output| {
        model.morphology.encode(output)
    })?;
    write_section(4, model.matrix.costs.len() as u64, &mut |output| {
        model.matrix.encode(output).map_err(Into::into)
    })?;
    write_section(5, model.characters.values.len() as u64, &mut |output| {
        model.characters.encode(output)
    })?;
    write_section(
        6,
        u64::from(crate::morphology::unicode::CODE_POINTS),
        &mut |output| model.unicode.encode(output).map_err(Into::into),
    )?;
    write_section(7, model.analysis.completion.len() as u64, &mut |output| {
        model.analysis.encode(output)
    })?;
    sections.push(Section {
        kind: 8,
        records: 1,
        bytes: canonical(&model.manifest)?,
    });
    let bytes = frame::encode(&sections, limits)?;
    drop(sections);
    drop(model);
    let dictionary = KuromojiDictionary::from_bytes(&bytes, limits)?;
    verify_dictionary(&dictionary, limits)?;
    Ok(bytes)
}
