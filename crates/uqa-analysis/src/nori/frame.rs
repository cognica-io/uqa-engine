//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Versioned section framing with bounded decoding and semantic content identity.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest, Sha256};

use super::error::{check_limit, invalid};
use super::io::{vector, Reader};
use super::{DictionaryError, DictionaryLimits, DictionaryResult};

const MAGIC: &[u8; 8] = b"UQANORI\0";
const VERSION: u32 = 1;
const SECTION_COUNT: usize = 7;
const HEADER_SIZE: usize = 56;
const DIRECTORY_SIZE: usize = 72;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DictionaryId([u8; 32]);

impl fmt::Display for DictionaryId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(formatter, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl FromStr for DictionaryId {
    type Err = DictionaryError;

    fn from_str(text: &str) -> DictionaryResult<Self> {
        if text.len() != 64 || !text.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(invalid(
                "content identity",
                "expected 64 hexadecimal digits",
            ));
        }
        let mut bytes = [0; 32];
        for (index, byte) in bytes.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&text[index * 2..index * 2 + 2], 16)
                .map_err(|_| invalid("content identity", "invalid hexadecimal byte"))?;
        }
        Ok(Self(bytes))
    }
}

impl Serialize for DictionaryId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for DictionaryId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

pub(super) struct Section {
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

fn identity(entries: &[DirectoryEntry]) -> DictionaryId {
    let mut hash = Sha256::new();
    hash.update(b"UQA Nori semantic dictionary\0");
    hash.update(VERSION.to_le_bytes());
    for entry in entries {
        hash.update(entry.kind.to_le_bytes());
        hash.update((entry.decoded as u64).to_le_bytes());
        hash.update(entry.records.to_le_bytes());
        hash.update(entry.hash);
    }
    DictionaryId(hash.finalize().into())
}

fn size(reader: &mut Reader<'_>) -> DictionaryResult<usize> {
    usize::try_from(reader.u64()?).map_err(|_| reader.invalid("size exceeds address space"))
}

pub(super) fn decode(
    bytes: &[u8],
    limits: DictionaryLimits,
) -> DictionaryResult<(DictionaryId, Vec<Section>)> {
    check_limit("encoded bytes", bytes.len(), limits.max_encoded_bytes)?;
    let mut reader = Reader::new(bytes, "bundle frame", false);
    if reader.take(8)? != MAGIC {
        return Err(reader.invalid("invalid magic"));
    }
    let version = reader.u32()?;
    if version != VERSION {
        return Err(DictionaryError::Version(version));
    }
    if reader.u32()? as usize != SECTION_COUNT {
        return Err(reader.invalid("missing or unknown sections"));
    }
    let total = size(&mut reader)?;
    check_limit("decoded bytes", total, limits.max_decoded_bytes)?;
    let id = DictionaryId(reader.array()?);
    let mut entries = vector(SECTION_COUNT)?;
    let mut offset = HEADER_SIZE + SECTION_COUNT * DIRECTORY_SIZE;
    let mut decoded = 0_usize;
    for index in 0..SECTION_COUNT {
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
    if identity(&entries) != id {
        return Err(DictionaryError::Checksum(0));
    }
    let mut sections = vector(SECTION_COUNT)?;
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
pub(super) fn encode(sections: &[Section], limits: DictionaryLimits) -> DictionaryResult<Vec<u8>> {
    use super::io::Writer;

    if sections.len() != SECTION_COUNT {
        return Err(invalid("bundle encoder", "incorrect section count"));
    }
    let total = sections
        .iter()
        .try_fold(0_usize, |total, section| {
            total.checked_add(section.bytes.len())
        })
        .ok_or_else(|| invalid("bundle encoder", "decoded size overflow"))?;
    check_limit("decoded bytes", total, limits.max_decoded_bytes)?;
    let mut entries = vector(SECTION_COUNT)?;
    let mut payloads = vector(SECTION_COUNT)?;
    let mut offset = HEADER_SIZE + SECTION_COUNT * DIRECTORY_SIZE;
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
    output.bytes(MAGIC)?;
    output.u32(VERSION)?;
    output.count(SECTION_COUNT)?;
    output.u64(total as u64)?;
    output.bytes(&identity(&entries).0)?;
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
