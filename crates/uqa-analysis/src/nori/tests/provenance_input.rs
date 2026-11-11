//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::fixtures;
use crate::morphology::manifest::tests::{verify_fields, verify_retirement_and_errors, Fields};
use crate::nori::dictionary::provenance::Provenance;
use crate::nori::DictionaryError;
use std::sync::Arc;
use uqa_core::{image_io::ImageDecoder, memory::MemoryBudget, CancellationToken};

fn input(
    bytes: Arc<Vec<u8>>,
    memory: &MemoryBudget,
    token: &CancellationToken,
) -> ImageDecoder<Fields, DictionaryError> {
    ImageDecoder::new(memory, token, |mut channel| async move {
        let value = Provenance::decode_with(&bytes, bytes.len(), &mut channel.input()).await?;
        Ok((value.json, value.scripts))
    })
    .unwrap()
}

fn ordinary(bytes: &[u8]) -> Result<Fields, DictionaryError> {
    let value = Provenance::decode(bytes, bytes.len())?;
    Ok((value.json, value.scripts))
}

#[test]
fn native_nori_provenance_retains_complete_canonical_fields_and_reference_validation() {
    verify_fields(fixtures::provenance(), input);
}

#[test]
fn native_nori_provenance_errors_and_interruption_retire_all_candidate_owners() {
    verify_retirement_and_errors(fixtures::provenance(), input, ordinary);
}
