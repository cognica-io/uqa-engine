//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Pair native sequence records with their exact captured authority before publishing a catalog.

use super::SequenceCatalog;
use crate::catalog::sequence::image::invalid;
use std::io::Read;
use uqa_core::{
    image_io::{decode_image, ImageDecodeWindow, ImageDecoder},
    memory::MemoryBudget,
    CancellationToken,
};
use uqa_sql::{
    catalog::{image::CatalogImageInput, security::sequence_registry::SequenceSecurities},
    SQLError,
};

mod metadata;
mod validation;

impl SequenceCatalog {
    /// Reconstruct an unpublished catalog under one allowance. Record and authority images must have identical ordered membership and persistence. Role, table/column and database-wide OID closure remain the enclosing restore owner's responsibility.
    pub fn read_images(
        metadata: &mut dyn Read,
        authorities: &mut dyn Read,
        memory: &MemoryBudget,
        cancellation: &CancellationToken,
    ) -> Result<Self, SQLError> {
        SequenceCatalogInput::read_metadata(metadata, memory, cancellation)?
            .read_authority(authorities, memory)
    }
}

enum State {
    Metadata(ImageDecoder<SequenceCatalog, SQLError>),
    Ready(SequenceCatalog),
    Authority {
        catalog: SequenceCatalog,
        input: CatalogImageInput<SequenceSecurities>,
    },
    Validation(ImageDecoder<SequenceCatalog, SQLError>),
}

/// Unpublished ordered input owns metadata, authority and membership validation. Byte/poll windows retain incomplete fields; no state exposes a usable catalog before both images and their pairing are complete. Errors, invalid operation order and unwinding retire the original candidate.
pub struct SequenceCatalogInput {
    state: Option<State>,
    mutations: MemoryBudget,
    cancellation: CancellationToken,
}

impl SequenceCatalogInput {
    pub fn new(
        memory: &MemoryBudget,
        mutations: &MemoryBudget,
        cancellation: &CancellationToken,
    ) -> Result<Self, SQLError> {
        Ok(Self {
            state: Some(State::Metadata(metadata::start(memory, cancellation)?)),
            mutations: mutations.clone(),
            cancellation: cancellation.clone(),
        })
    }

    /// Return true only when the complete metadata image admits the authority operation.
    pub fn advance_metadata(
        &mut self,
        reader: &mut dyn Read,
        window: ImageDecodeWindow,
    ) -> Result<bool, SQLError> {
        let Some(State::Metadata(mut input)) = self.state.take() else {
            return Err(invalid("metadata input order"));
        };
        self.cancellation.check()?;
        match input.advance(reader, window)? {
            Some(catalog) => {
                self.state = Some(State::Ready(catalog));
                Ok(true)
            }
            None => {
                self.state = Some(State::Metadata(input));
                Ok(false)
            }
        }
    }

    /// Continue authority decoding and then native membership validation within the supplied window. Validation uses checkpoints without additional source reads. Successful completion releases input cancellation and retains original payloads with the selected destination mutation allowance.
    pub fn advance_authority(
        &mut self,
        reader: &mut dyn Read,
        window: ImageDecodeWindow,
    ) -> Result<Option<SequenceCatalog>, SQLError> {
        let state = self
            .state
            .take()
            .ok_or_else(|| invalid("retired sequence input"))?;
        self.cancellation.check()?;
        if window.bytes == 0 || window.steps == 0 {
            return Err(invalid("zero sequence input limit"));
        }
        let state = match state {
            State::Ready(catalog) => {
                let input = SequenceSecurities::image_input(
                    catalog.budget(),
                    &self.mutations,
                    &self.cancellation,
                )?;
                State::Authority { catalog, input }
            }
            state => state,
        };
        match state {
            State::Authority { catalog, mut input } => {
                if let Some(security) = input.advance_with_window(reader, usize::MAX, window)? {
                    self.state = Some(State::Validation(validation::start(
                        catalog,
                        security,
                        &self.mutations,
                        &self.cancellation,
                    )?));
                } else {
                    self.state = Some(State::Authority { catalog, input });
                }
                Ok(None)
            }
            State::Validation(mut input) => {
                let result = input.advance(reader, window)?;
                if result.is_none() {
                    self.state = Some(State::Validation(input));
                }
                Ok(result)
            }
            _ => Err(invalid("authority input order")),
        }
    }

    /// Drive the same metadata continuation synchronously, leaving authority input unpublished.
    pub fn read_metadata(
        metadata: &mut dyn Read,
        memory: &MemoryBudget,
        cancellation: &CancellationToken,
    ) -> Result<Self, SQLError> {
        let catalog = decode_image(metadata, memory, cancellation, async |input| {
            metadata::read(input).await
        })?;
        Ok(Self {
            state: Some(State::Ready(catalog)),
            mutations: memory.clone(),
            cancellation: cancellation.clone(),
        })
    }

    /// Complete the same authority and membership continuations synchronously. The destination allowance is selected only before authority input begins.
    pub fn read_authority(
        mut self,
        authorities: &mut dyn Read,
        mutations: &MemoryBudget,
    ) -> Result<SequenceCatalog, SQLError> {
        let Some(State::Ready(catalog)) = self.state.take() else {
            return Err(invalid("authority input order"));
        };
        let memory = catalog.budget().clone();
        let security = SequenceSecurities::read_image_with_mutation_budget(
            authorities,
            &memory,
            mutations,
            &self.cancellation,
        )?;
        decode_image(
            &mut std::io::empty(),
            &memory,
            &self.cancellation,
            async |input| validation::read(catalog, security, mutations, input).await,
        )
    }
}
