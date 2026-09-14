//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Canonical analyzer identity is separate from dictionary artifact identity.

use std::{fmt, str::FromStr};

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest, Sha256};

use crate::{AnalysisError, AnalysisResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AnalyzerFingerprint([u8; 32]);

impl AnalyzerFingerprint {
    /// The binary identity carried by persistent index metadata.
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Construct an identity value; descriptor restoration still verifies its contents separately.
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub(super) fn digest(bytes: &[u8]) -> Self {
        let mut hash = Sha256::new();
        hash.update(b"UQA analyzer descriptor\0");
        hash.update(bytes);
        Self(hash.finalize().into())
    }
}

impl fmt::Display for AnalyzerFingerprint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(formatter, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl FromStr for AnalyzerFingerprint {
    type Err = AnalysisError;

    fn from_str(text: &str) -> AnalysisResult<Self> {
        if text.len() != 64 || !text.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(super::invalid("fingerprint requires 64 hexadecimal digits"));
        }
        let mut bytes = [0; 32];
        for (index, byte) in bytes.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&text[index * 2..index * 2 + 2], 16)
                .map_err(|_| super::invalid("invalid fingerprint byte"))?;
        }
        Ok(Self(bytes))
    }
}

impl Serialize for AnalyzerFingerprint {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for AnalyzerFingerprint {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}
