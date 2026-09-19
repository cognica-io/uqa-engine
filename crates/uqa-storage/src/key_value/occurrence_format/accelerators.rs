//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Scorer identity and finite bounds share one value, including native row projections.

use super::{other_error, BudgetedVec, StorageBackendResult, StorageReadControl};

/// A persisted block bound together with the exact scorer identity used to build it.
pub struct BlockMaxValue<'a> {
    pub score: f64,
    pub fingerprint: &'a str,
}

impl<'a> BlockMaxValue<'a> {
    pub fn decode(bytes: &'a [u8]) -> StorageBackendResult<Self> {
        if bytes.len() < 12 || &bytes[..4] != b"UBM1" {
            return Err(other_error("invalid occurrence block-max value"));
        }
        let score = f64::from_le_bytes(bytes[4..12].try_into().expect("checked score width"));
        crate::block_max_index::validate_score(score)?;
        let fingerprint = std::str::from_utf8(&bytes[12..])
            .map_err(|_| other_error("invalid block-max scorer identity"))?;
        Ok(Self { score, fingerprint })
    }

    pub fn encode(&self, control: &StorageReadControl) -> StorageBackendResult<BudgetedVec<u8>> {
        control.check()?;
        crate::block_max_index::validate_score(self.score)?;
        let mut bytes = BudgetedVec::new(control.memory());
        bytes.extend_from_slice(b"UBM1")?;
        bytes.extend_from_slice(&self.score.to_le_bytes())?;
        for chunk in self.fingerprint.as_bytes().chunks(1024) {
            control.check()?;
            bytes.extend_from_slice(chunk)?;
        }
        Ok(bytes)
    }
}
