//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Borrow encoded directories and retain only requested decoded occurrence allocations.

use super::{
    corrupt, occurrence_count, validate_header, visit_entry, DocId, OccurrencePosting,
    PostingScore, StorageBackendResult, TokenOccurrence, OCCURRENCE_FORMAT_VERSION,
    POSITIONS_MAGIC, SCORE_MAGIC,
};
use crate::clustered_postings::{
    positions::PositionDirectory,
    scores::{visit_score_block, ScoreDirectory},
};
use uqa_core::memory::{Budgeted, BudgetedVec, MemoryBudget, MemoryReservation};

struct Records {
    values: BudgetedVec<OccurrencePosting>,
    children: MemoryReservation,
}

/// Decode complete graph records while retaining their vector and every occurrence buffer.
///
/// Encoded payloads stay borrowed. Directory validation and decoding poll the callback; no temporary score or offset vectors are allocated. Failure drops all partial output reservations.
pub fn decode_occurrence_cluster_budgeted(
    cluster_id: u64,
    score_blob: &[u8],
    positions_blob: &[u8],
    budget: &MemoryBudget,
    mut poll: impl FnMut() -> StorageBackendResult<()>,
) -> StorageBackendResult<Budgeted<Vec<OccurrencePosting>>> {
    let (scores, positions) = directories(score_blob, positions_blob, &mut poll)?;
    let mut output = Records {
        values: BudgetedVec::new(budget),
        children: budget.empty_reservation(),
    };
    output.values.reserve(scores.count)?;
    visit_cluster(
        cluster_id,
        &scores,
        &positions,
        score_blob,
        &mut poll,
        |score, bytes, poll| {
            let decoded = decode_entry(bytes, score.term_freq, budget, poll)?;
            output.values.reserve(1)?;
            let (occurrences, memory) = decoded.into_parts();
            output.children.absorb(memory);
            output.values.push(OccurrencePosting {
                doc_id: score.doc_id,
                doc_length: score.doc_length,
                occurrences,
            })?;
            Ok(())
        },
    )?;
    poll()?;
    let (records, mut memory) = output.values.into_parts();
    memory.absorb(output.children);
    Ok(Budgeted::new(records, memory))
}

/// Validate the complete cluster and allocate occurrences only for the requested document.
///
/// Missing documents allocate no decoded buffers. Corruption in another document still rejects the read, and cancellation or quota failure publishes no partial record. The returned guard retains the selected occurrence vector.
pub fn decode_occurrence_document_budgeted(
    cluster_id: u64,
    score_blob: &[u8],
    positions_blob: &[u8],
    doc_id: DocId,
    budget: &MemoryBudget,
    mut poll: impl FnMut() -> StorageBackendResult<()>,
) -> StorageBackendResult<Option<Budgeted<OccurrencePosting>>> {
    let (scores, positions) = directories(score_blob, positions_blob, &mut poll)?;
    let mut output = None;
    visit_cluster(
        cluster_id,
        &scores,
        &positions,
        score_blob,
        &mut poll,
        |score, bytes, poll| {
            if score.doc_id == doc_id {
                let (occurrences, memory) =
                    decode_entry(bytes, score.term_freq, budget, poll)?.into_parts();
                output = Some(Budgeted::new(
                    OccurrencePosting {
                        doc_id,
                        doc_length: score.doc_length,
                        occurrences,
                    },
                    memory,
                ));
            } else {
                visit_entry(bytes, score.term_freq, poll, |_| Ok(()))?;
            }
            Ok(())
        },
    )?;
    poll()?;
    Ok(output)
}

fn directories<'a>(
    score_blob: &'a [u8],
    positions_blob: &'a [u8],
    poll: &mut dyn FnMut() -> StorageBackendResult<()>,
) -> StorageBackendResult<(ScoreDirectory<'a>, PositionDirectory<'a>)> {
    poll()?;
    validate_header(score_blob, *SCORE_MAGIC)?;
    validate_header(positions_blob, *POSITIONS_MAGIC)?;
    if score_blob[4] != OCCURRENCE_FORMAT_VERSION || positions_blob[4] != OCCURRENCE_FORMAT_VERSION
    {
        return Err(corrupt(
            "legacy positional data requires an atomic source rebuild",
        ));
    }
    let scores = ScoreDirectory::new(score_blob, poll)?;
    let positions = PositionDirectory::new(positions_blob, scores.count, poll)?;
    Ok((scores, positions))
}

fn visit_cluster(
    cluster_id: u64,
    scores: &ScoreDirectory<'_>,
    positions: &PositionDirectory<'_>,
    score_blob: &[u8],
    poll: &mut dyn FnMut() -> StorageBackendResult<()>,
    mut visit: impl FnMut(
        PostingScore,
        &[u8],
        &mut dyn FnMut() -> StorageBackendResult<()>,
    ) -> StorageBackendResult<()>,
) -> StorageBackendResult<()> {
    let mut ordinal = 0;
    let mut previous = None;
    for index in 0..scores.blocks {
        poll()?;
        visit_score_block(
            score_blob,
            cluster_id,
            scores.block(index)?,
            poll,
            |score, poll| {
                if previous.is_some_and(|doc_id| doc_id >= score.doc_id) {
                    return Err(corrupt("posting scores are not strictly ordered"));
                }
                previous = Some(score.doc_id);
                visit(score, positions.entry(ordinal)?, poll)?;
                ordinal += 1;
                Ok(())
            },
        )?;
    }
    poll()?;
    Ok(())
}

fn decode_entry(
    bytes: &[u8],
    frequency: u64,
    budget: &MemoryBudget,
    poll: &mut dyn FnMut() -> StorageBackendResult<()>,
) -> StorageBackendResult<Budgeted<Vec<TokenOccurrence>>> {
    poll()?;
    let count = occurrence_count(bytes, frequency)?;
    let mut occurrences = BudgetedVec::new(budget);
    occurrences.reserve(count)?;
    visit_entry(bytes, frequency, poll, |occurrence| {
        occurrences.push(occurrence)?;
        Ok(())
    })?;
    let (occurrences, memory) = occurrences.into_parts();
    Ok(Budgeted::new(occurrences, memory))
}
