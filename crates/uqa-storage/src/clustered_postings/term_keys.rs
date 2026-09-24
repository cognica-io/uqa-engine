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
    encode_term_key_refs(terms.iter())
}

pub(crate) fn encode_term_key_refs<'a>(
    terms: impl ExactSizeIterator<Item = &'a TokenTermKey> + Clone,
) -> StorageBackendResult<Vec<u8>> {
    let count = u32::try_from(terms.len())
        .map_err(|_| corrupt("document term key count exceeds the u32 on-disk format"))?;
    let mut size = 12_usize;
    let mut previous = None;
    for term in terms.clone() {
        if previous.is_some_and(|previous| previous >= term) {
            return Err(corrupt("document term keys are not strictly ordered"));
        }
        u32::try_from(term.as_bytes().len())
            .map_err(|_| corrupt("document term key length exceeds the u32 on-disk format"))?;
        size = size
            .checked_add(4)
            .and_then(|size| size.checked_add(term.as_bytes().len()))
            .ok_or(uqa_core::memory::MemoryError::SizeOverflow)?;
        previous = Some(term);
    }
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(size)
        .map_err(|error| super::StorageBackendError::Memory(error.into()))?;
    bytes.extend_from_slice(TERMS_MAGIC);
    bytes.extend_from_slice(&[OCCURRENCE_FORMAT_VERSION, 0, 0, 0]);
    bytes.extend_from_slice(&count.to_le_bytes());
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn borrowed_vocabulary_preserves_the_wire_format_and_order_checks() {
        let keys = BTreeMap::from([
            (TokenTermKey::from_text("a"), 1),
            (TokenTermKey::from_text("한국"), 2),
        ]);
        let encoded = encode_term_key_refs(keys.keys()).unwrap();
        let owned: Vec<_> = keys.into_keys().collect();
        assert_eq!(encoded, encode_term_keys(&owned).unwrap());
        assert_eq!(decode_term_keys(&encoded).unwrap(), owned);
        assert!(encode_term_key_refs(owned.iter().rev()).is_err());
        assert!(encode_term_key_refs([&owned[0], &owned[0]].into_iter()).is_err());
        assert_eq!(
            encode_term_key_refs([].into_iter()).unwrap(),
            encode_term_keys(&[]).unwrap(),
        );
    }
}
