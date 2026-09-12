//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Canonical binary posting keys for scalar strings and unpaired UTF-16 terms.

use uqa_analysis::TokenTerm;

use crate::{StorageBackendError, StorageBackendResult};

/// A tagged term key: zero plus UTF-8, or one plus big-endian UTF-16 containing unpaired units.
///
/// Scalar text has exactly one representation, including supplementary characters and the empty string. Byte ordering is the persistent vocabulary order; it is not linguistic collation. Field/table names remain separate key components.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TokenTermKey(Vec<u8>);

impl TokenTermKey {
    pub fn from_text(text: &str) -> Self {
        let mut bytes = Vec::with_capacity(text.len() + 1);
        bytes.push(0);
        bytes.extend_from_slice(text.as_bytes());
        Self(bytes)
    }

    pub fn from_term(term: &TokenTerm) -> Self {
        if let Some(text) = term.as_str() {
            return Self::from_text(text);
        }
        let units = term.utf16();
        let mut bytes = Vec::with_capacity(units.len() * 2 + 1);
        bytes.push(1);
        for unit in units.iter() {
            bytes.extend_from_slice(&unit.to_be_bytes());
        }
        Self(bytes)
    }

    pub fn from_bytes(bytes: Vec<u8>) -> StorageBackendResult<Self> {
        Self::validate(&bytes)?;
        Ok(Self(bytes))
    }

    /// Borrow scalar text when this key contains a valid Unicode string.
    pub fn as_str(&self) -> Option<&str> {
        (self.0[0] == 0).then(|| std::str::from_utf8(&self.0[1..]).expect("validated UTF-8 key"))
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.0
    }

    pub fn to_term(&self) -> TokenTerm {
        match self.0[0] {
            0 => TokenTerm::from(std::str::from_utf8(&self.0[1..]).expect("validated UTF-8 key")),
            1 => TokenTerm::from_utf16(
                self.0[1..]
                    .chunks_exact(2)
                    .map(|pair| u16::from_be_bytes([pair[0], pair[1]]))
                    .collect(),
            ),
            _ => unreachable!("validated token key tag"),
        }
    }

    fn validate(bytes: &[u8]) -> StorageBackendResult<()> {
        let invalid = || StorageBackendError::Other("invalid canonical token term key".into());
        match bytes.split_first() {
            Some((0, bytes)) => std::str::from_utf8(bytes)
                .map(|_| ())
                .map_err(|_| invalid()),
            Some((1, bytes)) if bytes.len() % 2 == 0 => {
                let units = bytes
                    .chunks_exact(2)
                    .map(|pair| u16::from_be_bytes([pair[0], pair[1]]));
                if !char::decode_utf16(units).any(|unit| unit.is_err()) {
                    return Err(invalid());
                }
                Ok(())
            }
            _ => Err(invalid()),
        }
    }
}

impl From<String> for TokenTermKey {
    fn from(text: String) -> Self {
        Self::from_text(&text)
    }
}

impl From<&str> for TokenTermKey {
    fn from(text: &str) -> Self {
        Self::from_text(text)
    }
}
