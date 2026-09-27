//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Sparse fingerprints preserve the same evaluated replacement and deletion selection as values.

use super::VectorRead;
use crate::{read_control::StorageReadControl, StorageBackendResult};
use sha2::{Digest, Sha256};
use uqa_core::DocId;

pub(crate) fn selected_fingerprint<'a>(
    dimensions: u32,
    base: Option<&dyn VectorRead>,
    selected: impl Iterator<Item = (DocId, Option<&'a dyn VectorRead>)>,
    control: &StorageReadControl,
) -> StorageBackendResult<Option<[u8; 32]>> {
    control.check()?;
    let base = match base {
        Some(base) => match base.corpus_fingerprint(control)? {
            Some(fingerprint) => Some(fingerprint),
            None => return Ok(None),
        },
        None => None,
    };
    let mut selected = selected.peekable();
    if base.is_some() && selected.peek().is_none() {
        return Ok(base);
    }
    let mut digest = Sha256::new();
    digest.update(b"uqa-selected-vectors-v1\0");
    digest.update(dimensions.to_le_bytes());
    match base {
        Some(fingerprint) => {
            digest.update([1]);
            digest.update(fingerprint);
        }
        None => digest.update([0]),
    }
    for (document, source) in selected {
        control.check()?;
        digest.update(document.to_le_bytes());
        match source {
            Some(source) => {
                let Some(fingerprint) = source.corpus_fingerprint(control)? else {
                    return Ok(None);
                };
                digest.update([1]);
                digest.update(fingerprint);
            }
            None => digest.update([0]),
        }
    }
    control.check()?;
    Ok(Some(digest.finalize().into()))
}
