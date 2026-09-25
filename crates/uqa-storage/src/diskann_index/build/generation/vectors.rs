//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{DiskANNBuildSink, DiskANNGenerationOptions};
use crate::diskann_index::build::{invalid, DiskANNBuildInput};
use crate::diskann_index::format::DiskANNArtifactDigests;
use crate::diskann_index::pages::DiskANNRecordKey;
use crate::diskann_index::{format::encode_codebook, NavigationInput, NavigationVector};
use crate::StorageBackendResult;
use sha2::{Digest, Sha256};
use uqa_core::memory::{BudgetedVec, MemoryError};

fn navigation(input: &DiskANNBuildInput, raw: &[f32]) -> StorageBackendResult<NavigationVector> {
    match NavigationInput::from_raw(input.dimensions, raw, &input.control)? {
        NavigationInput::Navigable(vector) => Ok(vector),
        NavigationInput::Exact(_) => Err(invalid(
            "generation vector changed navigation classification",
        )),
    }
}

pub(super) fn entry(input: &DiskANNBuildInput, seed: u64) -> StorageBackendResult<Option<u64>> {
    if input.node_count() == 0 {
        return Ok(None);
    }
    crate::diskann_index::vamana::select_entry(
        input.node_count(),
        input.dimensions as usize,
        seed,
        &input.control,
        &mut |node, visitor| {
            let raw = input.read_node(node)?;
            visitor(&navigation(input, raw.raw())?)
        },
    )
    .map(Some)
}

pub(super) fn quantize(
    input: &DiskANNBuildInput,
    width: usize,
    options: DiskANNGenerationOptions,
    sink: &mut dyn DiskANNBuildSink,
    artifacts: &mut DiskANNArtifactDigests,
) -> StorageBackendResult<()> {
    let Some(book) = input.train(width, options.training)? else {
        return Ok(());
    };
    let control = &input.control;
    let (bytes, identity) = encode_codebook(input.coverage.generation(), &book, control)?;
    control.check_value_size(bytes.len(), options.max_record_bytes)?;
    sink.write_record(
        DiskANNRecordKey::Codebook,
        &bytes,
        options.max_record_bytes,
        control,
    )?;
    artifacts.codebook = identity.codebook_digest();
    drop(bytes);
    let count = input.node_count().min(options.code_batch_nodes as u64) as usize;
    let capacity = count.checked_mul(width).ok_or(MemoryError::SizeOverflow)?;
    let mut codes = BudgetedVec::new(control.memory());
    codes.reserve(capacity)?;
    let mut first = 0;
    let mut current = 0;
    let mut hash = Sha256::new();
    input.navigation.visit(control, &mut |record| {
        let vector = navigation(input, record.raw())?;
        codes.extend_from_slice(&book.encode(&vector, control)?)?;
        current += 1;
        if codes.len() == capacity || current == input.node_count() {
            let bytes = identity.encode_codes(first, &codes, control)?;
            control.check_value_size(bytes.len(), options.max_record_bytes)?;
            for chunk in codes.chunks(4096) {
                control.check()?;
                hash.update(chunk);
            }
            sink.write_record(
                DiskANNRecordKey::Codes(first),
                &bytes,
                options.max_record_bytes,
                control,
            )?;
            first = current;
            codes.clear();
        }
        Ok(())
    })?;
    artifacts.codes = hash.finalize().into();
    Ok(())
}
