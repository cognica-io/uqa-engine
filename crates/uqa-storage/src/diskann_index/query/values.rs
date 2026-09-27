//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Complete fixed tensors keep their raw scores even when their physical definition has changed.

use super::invalid;
use crate::diskann_index::scoring::ScoringStatsExt;
use crate::{
    diskann_index::{scoring::selection::TopK, DiskANNScoringStats},
    read_control::StorageReadControl,
    vector_index::{
        cosine_similarity_controlled,
        query::{postings_from_unique_scores, VectorQueryBuffer},
        validate_threshold, validate_vector_values_controlled, VectorRead,
    },
    StorageBackendResult,
};
use uqa_core::{DocId, PostingList};

pub(super) fn visit(
    source: &dyn VectorRead,
    control: &StorageReadControl,
    visit: &mut dyn FnMut(DocId) -> StorageBackendResult<()>,
) -> StorageBackendResult<()> {
    source.check_control(control)?;
    let mut after = None;
    while let Some(document) = source.next_document_after(after, control)? {
        if after.is_some_and(|after| document <= after) {
            return Err(invalid("fixed vector cursor did not advance"));
        }
        source.check_control(control)?;
        visit(document)?;
        after = Some(document);
    }
    source.check_control(control)
}

pub(super) fn score(
    source: &dyn VectorRead,
    document: DocId,
    query: &[f32],
    control: &StorageReadControl,
) -> StorageBackendResult<(Option<f32>, u64)> {
    source.check_control(control)?;
    let count = source.document_vector_count(document, control)?;
    if count > u64::from(u32::MAX) + 1 {
        return Err(invalid(
            "fixed vector ordinal count exceeds its identity range",
        ));
    }
    let mut best: Option<f32> = None;
    for ordinal in 0..count {
        let vector = source
            .read_vector(document, ordinal as u32, control)?
            .ok_or_else(|| invalid("missing fixed vector ordinal"))?;
        validate_vector_values_controlled(source.dimensions(), &vector, Some(control))?;
        let score = cosine_similarity_controlled(query, &vector, || source.check_control(control))?;
        if best.is_none_or(|best| score.total_cmp(&best).is_gt()) {
            best = Some(score);
        }
    }
    source.check_control(control)?;
    Ok((best, count))
}

pub(super) fn exact(
    source: &dyn VectorRead,
    query: &[f32],
    k: usize,
    control: &StorageReadControl,
) -> StorageBackendResult<(PostingList, DiskANNScoringStats)> {
    let mut selected = TopK::new(k, control);
    let mut stats = DiskANNScoringStats::default();
    visit(source, control, &mut |document| {
        let (score, vectors) = score(source, document, query, control)?;
        stats.record(vectors)?;
        if let Some(score) = score {
            selected.offer_raw(document, score)?;
        }
        Ok(())
    })?;
    Ok((selected.finish(control)?, stats))
}

pub(super) fn threshold(
    source: &dyn VectorRead,
    query: &[f32],
    threshold: f32,
    control: &StorageReadControl,
) -> StorageBackendResult<(PostingList, DiskANNScoringStats)> {
    validate_threshold(threshold)?;
    let mut scores = VectorQueryBuffer::new(Some(control));
    let mut stats = DiskANNScoringStats::default();
    visit(source, control, &mut |document| {
        let (score, vectors) = score(source, document, query, control)?;
        stats.record(vectors)?;
        if let Some(score) = score.filter(|score| *score >= threshold) {
            scores.push((document, score))?;
        }
        Ok(())
    })?;
    Ok((
        postings_from_unique_scores(scores, None, Some(control))?,
        stats,
    ))
}

pub(super) fn count(
    source: &dyn VectorRead,
    control: &StorageReadControl,
) -> StorageBackendResult<usize> {
    let mut count = 0_usize;
    visit(source, control, &mut |document| {
        let vectors = usize::try_from(source.document_vector_count(document, control)?)
            .map_err(|_| uqa_core::memory::MemoryError::SizeOverflow)?;
        count = count
            .checked_add(vectors)
            .ok_or(uqa_core::memory::MemoryError::SizeOverflow)?;
        Ok(())
    })?;
    Ok(count)
}
