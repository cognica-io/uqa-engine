//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A consuming input owns partial corpus publication and all of its original leases.

use super::{charge, RetainedInvertedIndexBuilder};
use crate::{
    image_io::work::Work,
    inverted_index::{
        replacement::AdmittedNames, DocId, FieldName, IndexedFieldMetadata, MemoryFieldCounters,
        MemoryReplacementPlan, StagedMemoryDocument,
    },
    StorageBackendError, StorageBackendResult,
};
use std::sync::Arc;
use uqa_core::{
    image_io::{AsyncImageInput, ImageDecoder},
    memory::{Budgeted, MemoryError, MemoryReservation, OwnedMap},
};

pub(super) struct Publication {
    staged: StagedMemoryDocument,
    plan: MemoryReplacementPlan,
    memory: MemoryReservation,
    retained: usize,
}

impl RetainedInvertedIndexBuilder {
    /// Select one evaluated text document. Decode, validation and native publication share the caller's byte/poll window. Any failure or abandonment consumes the entire unpublished corpus; synchronous borrowed append retains its existing atomic rejection contract.
    pub fn document_input(
        self,
        id: DocId,
    ) -> StorageBackendResult<ImageDecoder<Self, StorageBackendError>> {
        self.control.check()?;
        if self.index.state.documents.contains_key(&id) {
            return Err(StorageBackendError::Other(
                "retained text input repeats a document".into(),
            ));
        }
        let control = self.control.clone();
        Ok(ImageDecoder::new(
            control.memory(),
            control.cancellation(),
            move |mut reader| async move {
                let mut owner = self;
                let mut input = reader.input();
                let staged = crate::inverted_index::image::read_from(id, &mut input).await?;
                let publication = owner.prepare_from(id, staged, &mut input).await?;
                owner.publish_from(id, publication, &mut input).await?;
                Ok(owner)
            },
        )?)
    }

    pub(super) async fn prepare_from(
        &self,
        id: DocId,
        staged: Budgeted<StagedMemoryDocument>,
        input: &mut AsyncImageInput<'_>,
    ) -> StorageBackendResult<Publication> {
        let plan = self.plan_from(id, &staged.fields, input).await?;
        let charge =
            charge::new_document_from(&self.index.state, &staged, &plan, &self.control, input)
                .await?;
        let entries = self.control.memory().reserve(charge.new_entries)?;
        self.control.check()?;
        let (staged, mut memory) = staged.into_parts();
        let (plan, plan_memory) = plan.into_parts();
        memory.absorb(plan_memory);
        memory.absorb(entries);
        Ok(Publication {
            staged,
            plan,
            memory,
            retained: charge.retained,
        })
    }

    pub(super) async fn plan_from(
        &self,
        id: DocId,
        fields: &OwnedMap<FieldName, IndexedFieldMetadata>,
        input: &mut AsyncImageInput<'_>,
    ) -> StorageBackendResult<Budgeted<MemoryReplacementPlan>> {
        let mut memory = self.control.memory().reserve(
            fields
                .len()
                .checked_mul(OwnedMap::<FieldName, MemoryFieldCounters>::entry_bytes())
                .ok_or(MemoryError::SizeOverflow)?,
        )?;
        let plan = self
            .index
            .state
            .plan_replacement_from(
                id,
                fields,
                AdmittedNames {
                    input,
                    memory: &mut memory,
                },
            )
            .await?;
        Ok(Budgeted::new(plan, memory))
    }

    pub(super) async fn publish_from(
        &mut self,
        id: DocId,
        publication: Publication,
        work: &mut impl Work,
    ) -> StorageBackendResult<()> {
        let Publication {
            staged,
            plan,
            memory,
            retained,
        } = publication;
        let released = memory.bytes() - retained;
        // The corpus owns every pending lease before the first node moves into it. A failed consuming continuation drops partial native state before releasing that allowance.
        self.memory.absorb(memory);
        Arc::get_mut(&mut self.index.state)
            .expect("unpublished text builder owns its corpus")
            .apply_replacement_from(id, staged, plan, work)
            .await?;
        drop(self.memory.split(released));
        Ok(())
    }
}
