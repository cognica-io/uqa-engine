//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bounded offline input for independently exported Lucene models.

use super::error::{check_limit, invalid};
use super::io::{vector, Reader};
use super::limits::DictionaryLimits;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::Read;
use std::path::Path;

#[derive(Debug, thiserror::Error)]
pub(crate) enum Error {
    #[error(transparent)]
    Dictionary(#[from] super::DictionaryError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Utf16(#[from] std::string::FromUtf16Error),
    #[error(transparent)]
    Manifest(#[from] serde_json::Error),
}

type Result<T> = std::result::Result<T, Error>;

pub(crate) fn read_file(path: &Path, limit: usize) -> Result<Vec<u8>> {
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

pub(crate) fn header<'a>(bytes: &'a [u8], magic: &[u8]) -> Result<Reader<'a>> {
    let mut reader = Reader::new(bytes, "neutral model", true);
    if reader.take(8)? != magic {
        return Err(reader.invalid("invalid neutral file magic").into());
    }
    Ok(reader)
}

pub(crate) fn text(reader: &mut Reader<'_>, limits: DictionaryLimits) -> Result<Option<String>> {
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
        return Err(reader.invalid("truncated UTF-16 string").into());
    }
    let mut units = vector(count)?;
    for _ in 0..count {
        units.push(reader.u16()?);
    }
    Ok(Some(String::from_utf16(&units)?))
}

pub(crate) fn required_text(reader: &mut Reader<'_>, limits: DictionaryLimits) -> Result<String> {
    text(reader, limits)?.ok_or_else(|| reader.invalid("required string is absent").into())
}

pub(crate) fn count32(count: usize) -> Result<u32> {
    u32::try_from(count).map_err(|_| invalid("neutral model", "count exceeds u32").into())
}

pub(crate) fn read_inputs(
    directory: &Path,
    manifest: &Value,
    files: &[&str],
    limits: DictionaryLimits,
) -> Result<BTreeMap<String, Vec<u8>>> {
    let listed = manifest["files"]
        .as_array()
        .ok_or_else(|| invalid("neutral model", "missing file inventory"))?;
    let expected: BTreeSet<_> = files.iter().copied().collect();
    let mut names = BTreeSet::new();
    let mut inputs = BTreeMap::new();
    let mut total = 0_usize;
    for file in listed {
        let name = file["path"]
            .as_str()
            .ok_or_else(|| invalid("neutral model", "invalid file path"))?;
        if !expected.contains(name) || !names.insert(name) {
            return Err(invalid("neutral model", "duplicate or unexpected input file").into());
        }
        let bytes = read_file(&directory.join(name), limits.max_decoded_bytes)?;
        total = total
            .checked_add(bytes.len())
            .ok_or_else(|| invalid("neutral model", "size overflow"))?;
        check_limit("neutral model bytes", total, limits.max_decoded_bytes)?;
        let hash = format!("{:x}", Sha256::digest(&bytes));
        if file["bytes"].as_u64() != Some(bytes.len() as u64) || file["sha256"] != hash {
            return Err(invalid("neutral model", "input file size or checksum differs").into());
        }
        inputs.insert(name.to_owned(), bytes);
    }
    if names != expected {
        return Err(invalid("neutral model", "incomplete input inventory").into());
    }
    let actual: BTreeSet<_> = std::fs::read_dir(directory)?
        .map(|entry| entry.map(|entry| entry.file_name()))
        .collect::<std::result::Result<_, _>>()?;
    let mut allowed: BTreeSet<_> = files.iter().map(std::ffi::OsString::from).collect();
    allowed.insert("model_manifest.json".into());
    if actual != allowed {
        return Err(invalid("neutral model", "unexpected files in input directory").into());
    }
    Ok(inputs)
}

pub(crate) mod hash;
