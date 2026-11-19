//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Canonical term validation retains its original bytes across work checkpoints.

use super::{StorageBackendError, StorageBackendResult, TokenTermKey};
use crate::image_io::work::Work;
use uqa_core::image_io::AsyncImageInput;

#[cfg(test)]
mod tests;

impl TokenTermKey {
    pub(crate) async fn from_bytes_from(
        bytes: Vec<u8>,
        input: &mut AsyncImageInput<'_>,
    ) -> StorageBackendResult<Self> {
        Self::validate_with(&bytes, input).await?;
        Ok(Self(bytes))
    }

    pub(super) async fn validate_with(
        bytes: &[u8],
        work: &mut impl Work,
    ) -> StorageBackendResult<()> {
        let invalid = || StorageBackendError::Other("invalid canonical token term key".into());
        match bytes.split_first() {
            Some((0, mut bytes)) => {
                while !bytes.is_empty() {
                    work.checkpoint().await?;
                    let end = bytes.len().min(4096);
                    let consumed = match std::str::from_utf8(&bytes[..end]) {
                        Ok(_) => end,
                        Err(error) if error.error_len().is_none() && end < bytes.len() => {
                            error.valid_up_to()
                        }
                        Err(_) => return Err(invalid()),
                    };
                    bytes = &bytes[consumed..];
                }
                Ok(())
            }
            Some((1, bytes)) if bytes.len() % 2 == 0 => {
                let units = bytes
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|pair| u16::from_be_bytes(*pair));
                for (index, unit) in char::decode_utf16(units).enumerate() {
                    if index % 1024 == 0 {
                        work.checkpoint().await?;
                    }
                    if unit.is_err() {
                        return Ok(());
                    }
                }
                Err(invalid())
            }
            _ => Err(invalid()),
        }
    }
}
