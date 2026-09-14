//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Streamed neutral-model reconstruction without retaining generated records.

use super::Result;
use crate::morphology::error::invalid;
use serde_json::Value;
use sha2::{Digest, Sha256};

pub(crate) struct NeutralHash {
    hash: Sha256,
    bytes: u64,
    expected_bytes: u64,
    expected_hash: String,
}

impl NeutralHash {
    pub(crate) fn bytes(&mut self, bytes: &[u8]) -> Result<()> {
        self.bytes = self
            .bytes
            .checked_add(bytes.len() as u64)
            .ok_or_else(|| invalid("neutral verification", "byte count overflow"))?;
        if self.bytes > self.expected_bytes {
            return Err(invalid(
                "neutral verification",
                "reconstructed data exceeds expected length",
            )
            .into());
        }
        self.hash.update(bytes);
        Ok(())
    }

    pub(crate) fn u32(&mut self, value: u32) -> Result<()> {
        self.bytes(&value.to_be_bytes())
    }
    pub(crate) fn i32(&mut self, value: i32) -> Result<()> {
        self.u32(value as u32)
    }
    pub(crate) fn u16(&mut self, value: u16) -> Result<()> {
        self.bytes(&value.to_be_bytes())
    }

    pub(crate) fn count(&mut self, value: usize) -> Result<()> {
        self.u32(
            u32::try_from(value)
                .map_err(|_| invalid("neutral verification", "count exceeds u32"))?,
        )
    }

    pub(crate) fn text(&mut self, value: Option<&str>) -> Result<()> {
        let Some(text) = value else {
            return self.i32(-1);
        };
        self.count(text.encode_utf16().count())?;
        for unit in text.encode_utf16() {
            self.u16(unit)?;
        }
        Ok(())
    }
}

impl NeutralHash {
    pub(crate) fn new(manifest: &Value, name: &str) -> Result<Self> {
        let files = manifest["files"]
            .as_array()
            .ok_or_else(|| invalid("neutral verification", "missing file inventory"))?;
        let expected = files
            .iter()
            .find(|file| file["path"] == name)
            .ok_or_else(|| invalid("neutral verification", "missing expected file"))?;
        let expected_bytes = expected["bytes"]
            .as_u64()
            .ok_or_else(|| invalid("neutral verification", "missing expected byte count"))?;
        let expected_hash = expected["sha256"]
            .as_str()
            .ok_or_else(|| invalid("neutral verification", "missing expected hash"))?
            .to_owned();
        Ok(Self {
            hash: Sha256::new(),
            bytes: 0,
            expected_bytes,
            expected_hash,
        })
    }

    pub(crate) fn finish(self) -> Result<()> {
        let digest = format!("{:x}", self.hash.finalize());
        if self.bytes != self.expected_bytes || self.expected_hash != digest {
            return Err(invalid(
                "neutral verification",
                "reconstructed model hash or length differs",
            )
            .into());
        }
        Ok(())
    }
}
