//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Versioned section framing with bounded decoding and semantic content identity.

use super::{DictionaryError, DictionaryLimits, DictionaryResult};
use crate::morphology::frame;
pub(super) use frame::Section;

const FORMAT: frame::Format = frame::Format {
    magic: b"UQANORI\0",
    version: 1,
    section_count: 7,
    identity_domain: b"UQA Nori semantic dictionary\0",
};

crate::morphology::identity::dictionary_digest!(DictionaryId, "content identity", DictionaryError);

pub(super) async fn decode_with(
    bytes: &[u8],
    limits: DictionaryLimits,
    work: &mut impl crate::morphology::work::Work,
) -> DictionaryResult<(DictionaryId, Vec<Section>)> {
    let (id, sections) = frame::decode_with(&FORMAT, bytes, limits, work).await?;
    Ok((DictionaryId(id), sections))
}

#[cfg(any(test, feature = "nori-tools"))]
pub(super) fn encode(sections: &[Section], limits: DictionaryLimits) -> DictionaryResult<Vec<u8>> {
    frame::encode(&FORMAT, sections, limits).map_err(Into::into)
}
