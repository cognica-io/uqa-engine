//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Admit and validate a complete evaluated document before publishing any postings.

use super::{
    invalid, DocId, StagedMemoryDocument, StorageBackendResult, StorageReadControl, MAGIC,
};
use crate::image_io::ImageInput as Input;
use crate::{
    inverted_index::{
        retained::copy_name, FieldName, IndexedFieldMetadata, MemoryPosting, PostingKey,
    },
    TokenTermKey,
};
use std::ops::Bound;
use uqa_analysis::TokenLengthPolicy;
use uqa_core::{
    memory::{Budgeted, BudgetedMap, BudgetedVec, MemoryError, MemoryReservation, OwnedMap},
    TokenOccurrence, TokenOffsets,
};

pub(super) fn read(
    id: DocId,
    reader: &mut dyn std::io::Read,
    control: &StorageReadControl,
) -> StorageBackendResult<Budgeted<StagedMemoryDocument>> {
    control.check()?;
    let mut input = Input { reader, control };
    if &input.array::<8>()? != MAGIC {
        return Err(invalid());
    }
    let count = input.count()?;
    if count == 0 {
        return Err(invalid());
    }
    let mut staged = Staging {
        fields: OwnedMap::new(),
        terms: BudgetedVec::new(control.memory()),
        postings: BudgetedVec::new(control.memory()),
        payload: control.memory().reserve(
            count
                .checked_mul(OwnedMap::<FieldName, IndexedFieldMetadata>::entry_bytes())
                .ok_or(MemoryError::SizeOverflow)?,
        )?,
    };
    for _ in 0..count {
        staged.field(id, &mut input)?;
    }
    input.end()?;
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
    fn field(&mut self, id: DocId, input: &mut Input<'_>) -> StorageBackendResult<()> {
        let (name, memory) = input.bytes()?.into_parts();
        self.payload.absorb(memory);
        let name = String::from_utf8(name).map_err(|_| invalid())?;
        if self
            .fields
            .first_from(Bound::Included(name.as_str()))
            .is_some()
        {
            return Err(invalid());
        }
        let metadata = IndexedFieldMetadata::from_bytes(&input.array::<84>()?)?;
        let term_count = input.count()?;
        self.terms.reserve(term_count)?;
        self.postings.reserve(term_count)?;
        let mut positions = BudgetedMap::new(input.control.memory());
        let mut emitted = 0_u64;
        for _ in 0..term_count {
            let (bytes, memory) = input.bytes()?.into_parts();
            self.payload.absorb(memory);
            let term = TokenTermKey::from_bytes(bytes)?;
            if let Some((previous_field, previous_term)) = self.terms.last() {
                if previous_field == &name
                    && !previous_term
                        .cmp_with_control(&term, &mut || input.control.check())?
                        .is_lt()
                {
                    return Err(invalid());
                }
            }
            let count = input.count()?;
            if count == 0 {
                return Err(invalid());
            }
            let mut occurrences = BudgetedVec::new(input.control.memory());
            occurrences.reserve(count)?;
            for _ in 0..count {
                let occurrence = read_occurrence(input)?;
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
            let reverse_name = copy_name(&name, &mut self.payload, input.control)?;
            let (reverse_term, memory) = term
                .clone_budgeted(input.control.memory(), || input.control.check())?
                .into_parts();
            self.payload.absorb(memory);
            self.terms.push((reverse_name, reverse_term))?;
            let forward_name = copy_name(&name, &mut self.payload, input.control)?;
            self.postings
                .push(((forward_name, term), MemoryPosting::new(id, occurrences)))?;
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

fn read_occurrence(input: &mut Input<'_>) -> StorageBackendResult<TokenOccurrence> {
    let position = u32::from_le_bytes(input.array()?);
    let position_length = u32::from_le_bytes(input.array()?);
    let offsets = match input.array::<1>()?[0] {
        0 => None,
        1 => Some(TokenOffsets {
            start_utf8: u64::from_le_bytes(input.array()?),
            end_utf8: u64::from_le_bytes(input.array()?),
            start_utf16: u64::from_le_bytes(input.array()?),
            end_utf16: u64::from_le_bytes(input.array()?),
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
