//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Offline Nori packing and complete decoded-model verification.

use std::io::Write;
use std::path::{Path, PathBuf};

use uqa_analysis::nori::{pack, DictionaryLimits, NoriDictionary};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let arguments: Vec<_> = std::env::args_os().skip(1).collect();
    let limits = DictionaryLimits::default();
    match arguments.as_slice() {
        [command, source, output] if command == "pack" => {
            let output = PathBuf::from(output);
            if output.exists() {
                return Err("output already exists".into());
            }
            let bytes = pack::pack_directory(&PathBuf::from(source), limits)?;
            let dictionary = NoriDictionary::from_bytes(&bytes, limits)?;
            let parent = output
                .parent()
                .filter(|path| !path.as_os_str().is_empty())
                .unwrap_or_else(|| Path::new("."));
            let mut file = tempfile::NamedTempFile::new_in(parent)?;
            file.write_all(&bytes)?;
            file.as_file().sync_all()?;
            file.persist_noclobber(&output)?;
            println!(
                "Packed {} bytes; {} surfaces; {} words; identity {}",
                bytes.len(),
                dictionary.surface_count(),
                dictionary.word_count(),
                dictionary.id()
            );
        }
        [command, source] if command == "verify" => {
            let dictionary = pack::verify_bundle_file(&PathBuf::from(source), limits)?;
            println!(
                "Verified all neutral model values; identity {}",
                dictionary.id()
            );
        }
        _ => {
            return Err(
                "usage: pack_nori pack <neutral-directory> <bundle> | verify <bundle>".into(),
            )
        }
    }
    Ok(())
}
