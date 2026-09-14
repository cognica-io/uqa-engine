//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Versioned section framing with bounded decoding and semantic content identity.

use sha2::{Digest, Sha256};

use super::error::{check_limit, invalid};
use super::io::{vector, Reader};
use super::limits::DictionaryLimits;
use super::{DictionaryError, DictionaryResult};

const HEADER_SIZE: usize = 56;
const DIRECTORY_SIZE: usize = 72;

pub(crate) struct Format {
    pub magic: &'static [u8; 8],
    pub version: u32,
    pub section_count: usize,
    pub identity_domain: &'static [u8],
}

pub(crate) struct Section {
    pub kind: u32,
    pub records: u64,
    pub bytes: Vec<u8>,
}

struct DirectoryEntry {
    kind: u32,
    codec: u32,
    offset: usize,
    stored: usize,
    decoded: usize,
    records: u64,
    hash: [u8; 32],
}

fn identity(format: &Format, entries: &[DirectoryEntry]) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(format.identity_domain);
    hash.update(format.version.to_le_bytes());
    for entry in entries {
        hash.update(entry.kind.to_le_bytes());
        hash.update((entry.decoded as u64).to_le_bytes());
        hash.update(entry.records.to_le_bytes());
        hash.update(entry.hash);
    }
    hash.finalize().into()
}

fn size(reader: &mut Reader<'_>) -> DictionaryResult<usize> {
    usize::try_from(reader.u64()?).map_err(|_| reader.invalid("size exceeds address space"))
}

pub(crate) fn decode(
    format: &Format,
    bytes: &[u8],
    limits: DictionaryLimits,
) -> DictionaryResult<([u8; 32], Vec<Section>)> {
    check_limit("encoded bytes", bytes.len(), limits.max_encoded_bytes)?;
    let mut reader = Reader::new(bytes, "bundle frame", false);
    if reader.take(8)? != format.magic {
        return Err(reader.invalid("invalid magic"));
    }
    let version = reader.u32()?;
    if version != format.version {
        return Err(DictionaryError::Version(version));
    }
    if reader.u32()? as usize != format.section_count {
        return Err(reader.invalid("missing or unknown sections"));
    }
    let total = size(&mut reader)?;
    check_limit("decoded bytes", total, limits.max_decoded_bytes)?;
    let id = reader.array::<32>()?;
    let mut entries = vector(format.section_count)?;
    let mut offset = HEADER_SIZE + format.section_count * DIRECTORY_SIZE;
    let mut decoded = 0_usize;
    for index in 0..format.section_count {
        let entry = DirectoryEntry {
            kind: reader.u32()?,
            codec: reader.u32()?,
            offset: size(&mut reader)?,
            stored: size(&mut reader)?,
            decoded: size(&mut reader)?,
            records: reader.u64()?,
            hash: reader.array()?,
        };
        if entry.kind as usize != index + 1 || entry.codec > 1 || entry.offset != offset {
            return Err(reader.invalid("invalid section order, codec, or offset"));
        }
        if entry.codec == 0 && entry.stored != entry.decoded {
            return Err(reader.invalid("uncompressed section lengths differ"));
        }
        decoded = decoded
            .checked_add(entry.decoded)
            .ok_or_else(|| reader.invalid("decoded size overflow"))?;
        check_limit("decoded bytes", decoded, limits.max_decoded_bytes)?;
        offset = offset
            .checked_add(entry.stored)
            .ok_or_else(|| reader.invalid("stored size overflow"))?;
        if offset > bytes.len() {
            return Err(reader.invalid("section exceeds input"));
        }
        entries.push(entry);
    }
    if decoded != total || offset != bytes.len() {
        return Err(reader.invalid("size totals differ or trailing data is present"));
    }
    if identity(format, &entries) != id {
        return Err(DictionaryError::Checksum(0));
    }
    let mut sections = vector(format.section_count)?;
    for entry in entries {
        let stored = &bytes[entry.offset..entry.offset + entry.stored];
        let mut data = vector(entry.decoded)?;
        if entry.codec == 0 {
            data.extend_from_slice(stored);
        } else {
            data.resize(entry.decoded, 0);
            let mut state = miniz_oxide::inflate::stream::InflateState::new_boxed(
                miniz_oxide::DataFormat::Zlib,
            );
            let result = miniz_oxide::inflate::stream::inflate(
                &mut state,
                stored,
                &mut data,
                miniz_oxide::MZFlush::Finish,
            );
            if result.status != Ok(miniz_oxide::MZStatus::StreamEnd)
                || result.bytes_written != data.len()
                || result.bytes_consumed != stored.len()
            {
                return Err(invalid(
                    "compressed section",
                    "invalid or incorrectly sized zlib stream",
                ));
            }
        }
        let hash: [u8; 32] = Sha256::digest(&data).into();
        if hash != entry.hash {
            return Err(DictionaryError::Checksum(entry.kind));
        }
        sections.push(Section {
            kind: entry.kind,
            records: entry.records,
            bytes: data,
        });
    }
    Ok((id, sections))
}

#[cfg(any(test, feature = "nori-tools"))]
pub(crate) fn encode(
    format: &Format,
    sections: &[Section],
    limits: DictionaryLimits,
) -> DictionaryResult<Vec<u8>> {
    use crate::morphology::io::Writer;

    if sections.len() != format.section_count {
        return Err(invalid("bundle encoder", "incorrect section count"));
    }
    let total = sections
        .iter()
        .try_fold(0_usize, |total, section| {
            total.checked_add(section.bytes.len())
        })
        .ok_or_else(|| invalid("bundle encoder", "decoded size overflow"))?;
    check_limit("decoded bytes", total, limits.max_decoded_bytes)?;
    let mut entries = vector(format.section_count)?;
    let mut payloads = vector(format.section_count)?;
    let mut offset = HEADER_SIZE + format.section_count * DIRECTORY_SIZE;
    for (index, section) in sections.iter().enumerate() {
        if section.kind as usize != index + 1 {
            return Err(invalid("bundle encoder", "sections are not in order"));
        }
        let mut data = miniz_oxide::deflate::compress_to_vec_zlib(&section.bytes, 9);
        let codec = if data.len() < section.bytes.len() {
            1
        } else {
            data.clear();
            data.extend_from_slice(&section.bytes);
            0
        };
        entries.push(DirectoryEntry {
            kind: section.kind,
            codec,
            offset,
            stored: data.len(),
            decoded: section.bytes.len(),
            records: section.records,
            hash: Sha256::digest(&section.bytes).into(),
        });
        offset = offset
            .checked_add(data.len())
            .ok_or_else(|| invalid("bundle encoder", "size overflow"))?;
        check_limit("encoded bytes", offset, limits.max_encoded_bytes)?;
        payloads.push(data);
    }
    let mut output = Writer::default();
    output.0.try_reserve_exact(offset)?;
    output.bytes(format.magic)?;
    output.u32(format.version)?;
    output.count(format.section_count)?;
    output.u64(total as u64)?;
    output.bytes(&identity(format, &entries))?;
    for entry in entries {
        output.u32(entry.kind)?;
        output.u32(entry.codec)?;
        output.u64(entry.offset as u64)?;
        output.u64(entry.stored as u64)?;
        output.u64(entry.decoded as u64)?;
        output.u64(entry.records)?;
        output.bytes(&entry.hash)?;
    }
    for payload in payloads {
        output.bytes(&payload)?;
    }
    Ok(output.0)
}
