//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Admit and validate a complete evaluated document before publishing any postings.

use super::{
    invalid, DocId, StagedMemoryDocument, StorageBackendResult, StorageReadControl, MAGIC,
};
use crate::{
    inverted_index::{FieldName, IndexedFieldMetadata, MemoryPosting, PostingKey},
    TokenTermKey,
};
use std::ops::Bound;
use uqa_analysis::TokenLengthPolicy;
use uqa_core::image_io::{decode_image, AsyncImageInput as Input};
use uqa_core::{
    memory::{Budgeted, BudgetedMap, BudgetedVec, MemoryError, MemoryReservation, OwnedMap},
    TokenOccurrence, TokenOffsets,
};

pub(super) fn read(
    id: DocId,
    reader: &mut dyn std::io::Read,
    control: &StorageReadControl,
) -> StorageBackendResult<Budgeted<StagedMemoryDocument>> {
    decode_image(
        reader,
        control.memory(),
        control.cancellation(),
        async |mut input| read_from(id, &mut input).await,
    )
}

pub(in crate::inverted_index) async fn read_from(
    id: DocId,
    input: &mut Input<'_>,
) -> StorageBackendResult<Budgeted<StagedMemoryDocument>> {
    input.cancellation.check()?;
    if &input.array::<8>().await? != MAGIC {
        return Err(invalid());
    }
    let count = input.count().await?;
    if count == 0 {
        return Err(invalid());
    }
    let mut staged = Staging {
        fields: OwnedMap::new(),
        terms: BudgetedVec::new(input.memory),
        postings: BudgetedVec::new(input.memory),
        payload: input.memory.reserve(
            count
                .checked_mul(OwnedMap::<FieldName, IndexedFieldMetadata>::entry_bytes())
                .ok_or(MemoryError::SizeOverflow)?,
        )?,
    };
    for _ in 0..count {
        staged.field(id, input).await?;
    }
    input.end().await?;
    let (terms, memory) = staged.terms.into_parts();
    staged.payload.absorb(memory);
    let (postings, memory) = staged.postings.into_parts();
    staged.payload.absorb(memory);
    Ok(Budgeted::new(
        StagedMemoryDocument {
            fields: staged.fields,
            terms,
            postings,
        },
        staged.payload,
    ))
}

struct Staging {
    fields: OwnedMap<FieldName, IndexedFieldMetadata>,
    terms: BudgetedVec<PostingKey>,
    postings: BudgetedVec<(PostingKey, MemoryPosting)>,
    payload: MemoryReservation,
}

impl Staging {
    async fn field(&mut self, id: DocId, input: &mut Input<'_>) -> StorageBackendResult<()> {
        let (name, memory) = input.text().await?.into_parts();
        self.payload.absorb(memory);
        if self
            .fields
            .first_from(Bound::Included(name.as_str()))
            .is_some()
        {
            return Err(invalid());
        }
        let metadata = IndexedFieldMetadata::from_bytes(&input.array::<84>().await?)?;
        let term_count = input.count().await?;
        self.terms.reserve(term_count)?;
        self.postings.reserve(term_count)?;
        let mut positions = BudgetedMap::new(input.memory);
        let mut emitted = 0_u64;
        for ordinal in 0..term_count {
            let (bytes, memory) = input.bytes().await?.into_parts();
            self.payload.absorb(memory);
            let term = TokenTermKey::from_bytes_from(bytes, input).await?;
            if ordinal != 0 {
                let (_, previous_term) = self.terms.last().expect("previous term in this field");
                if !previous_term.cmp_from(&term, input).await?.is_lt() {
                    return Err(invalid());
                }
            }
            let count = input.count().await?;
            if count == 0 {
                return Err(invalid());
            }
            let mut occurrences = BudgetedVec::new(input.memory);
            occurrences.reserve(count)?;
            for _ in 0..count {
                let occurrence = read_occurrence(input).await?;
                if occurrences
                    .last()
                    .is_some_and(|previous: &TokenOccurrence| {
                        previous.position > occurrence.position
                    })
                    || occurrence.offsets.is_some_and(|offsets| {
                        offsets.end_utf8 > metadata.final_offsets.end_utf8
                            || offsets.end_utf16 > metadata.final_offsets.end_utf16
                    })
                {
                    return Err(invalid());
                }
                emitted = emitted.checked_add(1).ok_or(MemoryError::SizeOverflow)?;
                if metadata.length_policy == TokenLengthPolicy::DiscountOverlaps
                    && !positions.contains_key(&occurrence.position)
                {
                    positions.insert(occurrence.position, ())?;
                }
                occurrences.push(occurrence)?;
            }
            let (occurrences, memory) = occurrences.into_parts();
            self.payload.absorb(memory);
            let (reverse_name, memory) = input.copy_text(&name).await?.into_parts();
            self.payload.absorb(memory);
            let (reverse_term, memory) = term.clone_budgeted_from(input).await?.into_parts();
            self.payload.absorb(memory);
            self.terms.push((reverse_name, reverse_term))?;
            let (forward_name, memory) = input.copy_text(&name).await?.into_parts();
            self.payload.absorb(memory);
            self.postings.push((
                (forward_name, term),
                MemoryPosting {
                    doc_id: id,
                    occurrences,
                },
            ))?;
        }
        let length = match metadata.length_policy {
            TokenLengthPolicy::EmittedTokens => emitted,
            TokenLengthPolicy::DiscountOverlaps => {
                u64::try_from(positions.len()).map_err(|_| MemoryError::SizeOverflow)?
            }
        };
        if metadata.length != length {
            return Err(invalid());
        }
        self.fields.insert(name, metadata);
        Ok(())
    }
}

async fn read_occurrence(input: &mut Input<'_>) -> StorageBackendResult<TokenOccurrence> {
    let position = u32::from_le_bytes(input.array().await?);
    let position_length = u32::from_le_bytes(input.array().await?);
    let offsets = match input.array::<1>().await?[0] {
        0 => None,
        1 => Some(TokenOffsets {
            start_utf8: u64::from_le_bytes(input.array().await?),
            end_utf8: u64::from_le_bytes(input.array().await?),
            start_utf16: u64::from_le_bytes(input.array().await?),
            end_utf16: u64::from_le_bytes(input.array().await?),
        }),
        _ => return Err(invalid()),
    };
    let occurrence = TokenOccurrence {
        position,
        position_length,
        offsets,
    };
    occurrence.validate().map_err(|_| invalid())?;
    Ok(occurrence)
}
