//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Lossless reverse-document vocabulary with an explicit binary-term format version.

use crate::TokenTermKey;

use super::{
    corrupt, put_u32, read_u32, StorageBackendResult, OCCURRENCE_FORMAT_VERSION, TERMS_MAGIC,
};

pub fn encode_term_keys(terms: &[TokenTermKey]) -> StorageBackendResult<Vec<u8>> {
    if terms.windows(2).any(|pair| pair[0] >= pair[1]) {
        return Err(corrupt("document term keys are not strictly ordered"));
    }
    let mut bytes = Vec::new();
    bytes.extend_from_slice(TERMS_MAGIC);
    bytes.extend_from_slice(&[OCCURRENCE_FORMAT_VERSION, 0, 0, 0]);
    put_u32(&mut bytes, terms.len(), "document term key count")?;
    for term in terms {
        put_u32(
            &mut bytes,
            term.as_bytes().len(),
            "document term key length",
        )?;
        bytes.extend_from_slice(term.as_bytes());
    }
    Ok(bytes)
}

pub fn decode_term_keys(bytes: &[u8]) -> StorageBackendResult<Vec<TokenTermKey>> {
    if bytes.len() < 12 || bytes.get(..4) != Some(TERMS_MAGIC.as_slice()) {
        return Err(corrupt("missing document term key header"));
    }
    if bytes[4] != OCCURRENCE_FORMAT_VERSION || bytes[5..8] != [0; 3] {
        return Err(corrupt("unsupported document term key format"));
    }
    let count = read_u32(bytes, 8)? as usize;
    if count > (bytes.len() - 12) / 5 {
        return Err(corrupt("document term key count exceeds payload length"));
    }
    let mut remaining = &bytes[12..];
    let mut terms = Vec::with_capacity(count);
    for _ in 0..count {
        let length = read_u32(remaining, 0)? as usize;
        remaining = &remaining[4..];
        let key = remaining
            .get(..length)
            .ok_or_else(|| corrupt("truncated document term key"))?;
        let term = TokenTermKey::from_bytes(key.to_vec())?;
        if terms.last().is_some_and(|previous| previous >= &term) {
            return Err(corrupt("document term keys are not strictly ordered"));
        }
        terms.push(term);
        remaining = &remaining[length..];
    }
    if !remaining.is_empty() {
        return Err(corrupt("document term keys contain trailing bytes"));
    }
    Ok(terms)
}
