//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Versioned field ownership and original-source end metadata for persistent graph indexes.

use uqa_analysis::{AnalyzerFingerprint, CompiledAnalyzer, TokenLengthPolicy};
use uqa_core::TokenOffsets;

use super::IndexedFieldMetadata;
use crate::{StorageBackendError, StorageBackendResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IndexedFieldRevision {
    pub analyzer_fingerprint: AnalyzerFingerprint,
    pub occurrence_format_version: u8,
    pub length_policy: TokenLengthPolicy,
}

fn corrupt() -> StorageBackendError {
    StorageBackendError::Other("invalid indexed-field metadata".into())
}

impl IndexedFieldRevision {
    pub fn new(analyzer: &CompiledAnalyzer) -> Self {
        Self {
            analyzer_fingerprint: analyzer.descriptor().fingerprint(),
            occurrence_format_version: crate::clustered_postings::OCCURRENCE_FORMAT_VERSION,
            length_policy: analyzer.descriptor().length_policy(),
        }
    }

    pub fn to_bytes(self) -> StorageBackendResult<[u8; 40]> {
        if self.occurrence_format_version != crate::clustered_postings::OCCURRENCE_FORMAT_VERSION {
            return Err(corrupt());
        }
        let mut bytes = [0; 40];
        bytes[..4].copy_from_slice(b"UQIR");
        bytes[4] = 1;
        bytes[5] = self.occurrence_format_version;
        bytes[6] = match self.length_policy {
            TokenLengthPolicy::EmittedTokens => 0,
            TokenLengthPolicy::DiscountOverlaps => 1,
        };
        bytes[8..].copy_from_slice(self.analyzer_fingerprint.as_bytes());
        Ok(bytes)
    }

    pub fn from_bytes(bytes: &[u8]) -> StorageBackendResult<Self> {
        if bytes.len() != 40
            || &bytes[..4] != b"UQIR"
            || bytes[4] != 1
            || bytes[5] != crate::clustered_postings::OCCURRENCE_FORMAT_VERSION
            || bytes[7] != 0
        {
            return Err(corrupt());
        }
        let length_policy = match bytes[6] {
            0 => TokenLengthPolicy::EmittedTokens,
            1 => TokenLengthPolicy::DiscountOverlaps,
            _ => return Err(corrupt()),
        };
        Ok(Self {
            analyzer_fingerprint: AnalyzerFingerprint::from_bytes(
                bytes[8..].try_into().map_err(|_| corrupt())?,
            ),
            occurrence_format_version: bytes[5],
            length_policy,
        })
    }
}

impl IndexedFieldMetadata {
    pub fn revision(self) -> IndexedFieldRevision {
        IndexedFieldRevision {
            analyzer_fingerprint: self.analyzer_fingerprint,
            occurrence_format_version: self.occurrence_format_version,
            length_policy: self.length_policy,
        }
    }

    pub fn to_bytes(self) -> StorageBackendResult<[u8; 84]> {
        if self.final_offsets.start_utf8 > self.final_offsets.end_utf8
            || self.final_offsets.start_utf16 > self.final_offsets.end_utf16
        {
            return Err(corrupt());
        }
        let mut bytes = [0; 84];
        bytes[..40].copy_from_slice(&self.revision().to_bytes()?);
        bytes[..4].copy_from_slice(b"UQIM");
        for (slot, value) in bytes[40..80].chunks_exact_mut(8).zip([
            self.length,
            self.final_offsets.start_utf8,
            self.final_offsets.end_utf8,
            self.final_offsets.start_utf16,
            self.final_offsets.end_utf16,
        ]) {
            slot.copy_from_slice(&value.to_le_bytes());
        }
        bytes[80..].copy_from_slice(&self.final_position_increment.to_le_bytes());
        Ok(bytes)
    }

    pub fn from_bytes(bytes: &[u8]) -> StorageBackendResult<Self> {
        if bytes.len() != 84 || &bytes[..4] != b"UQIM" {
            return Err(corrupt());
        }
        let mut prefix = [0; 40];
        prefix.copy_from_slice(&bytes[..40]);
        prefix[..4].copy_from_slice(b"UQIR");
        let revision = IndexedFieldRevision::from_bytes(&prefix)?;
        let number = |offset: usize| -> StorageBackendResult<u64> {
            Ok(u64::from_le_bytes(
                bytes[offset..offset + 8]
                    .try_into()
                    .map_err(|_| corrupt())?,
            ))
        };
        let metadata = Self {
            analyzer_fingerprint: revision.analyzer_fingerprint,
            occurrence_format_version: revision.occurrence_format_version,
            length_policy: revision.length_policy,
            length: number(40)?,
            final_offsets: TokenOffsets {
                start_utf8: number(48)?,
                end_utf8: number(56)?,
                start_utf16: number(64)?,
                end_utf16: number(72)?,
            },
            final_position_increment: u32::from_le_bytes(
                bytes[80..].try_into().map_err(|_| corrupt())?,
            ),
        };
        metadata.to_bytes()?;
        Ok(metadata)
    }
}
