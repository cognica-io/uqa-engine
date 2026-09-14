//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Versioned section framing with bounded decoding and semantic content identity.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::error::invalid;
use super::{DictionaryError, DictionaryLimits, DictionaryResult};
use crate::morphology::frame;
pub(super) use frame::Section;

const FORMAT: frame::Format = frame::Format {
    magic: b"UQAKURO\0",
    version: 1,
    section_count: 8,
    identity_domain: b"UQA Kuromoji semantic dictionary\0",
};

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

pub(super) fn decode(
    bytes: &[u8],
    limits: DictionaryLimits,
) -> DictionaryResult<(DictionaryId, Vec<Section>)> {
    let (id, sections) = frame::decode(&FORMAT, bytes, limits)?;
    Ok((DictionaryId(id), sections))
}

#[cfg(any(test, feature = "kuromoji-tools"))]
pub(super) fn encode(sections: &[Section], limits: DictionaryLimits) -> DictionaryResult<Vec<u8>> {
    frame::encode(&FORMAT, sections, limits).map_err(Into::into)
}
