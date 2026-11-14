//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retained persistent definitions, authority and recorded OIDs form one private sequence generation.

use super::{SequenceCatalog, SequenceRecord};
use std::sync::Arc;
use uqa_core::{
    image_io::{AsyncImageInput, ImageDecodeWindow, ImageDecoder},
    memory::{Budgeted, BudgetedVec, MemoryBudget, MemoryReservation, ProductionControl},
    RelationIdentity,
};
use uqa_sql::{catalog::roles::RoleDefinitions, SQLError};
use uqa_storage::{
    catalog::{input::CatalogInputLimits, sequence_load::CatalogSequenceRow},
    read_control::StorageReadControl,
    CatalogFacade, StorageBackendError, StorageBackendResult,
};

mod oids;
mod rows;

struct Rows<T> {
    entries: std::vec::IntoIter<T>,
    _memory: MemoryReservation,
}

impl<T> From<BudgetedVec<T>> for Rows<T> {
    fn from(rows: BudgetedVec<T>) -> Self {
        let (entries, memory) = rows.into_parts();
        Self {
            entries: entries.into_iter(),
            _memory: memory,
        }
    }
}

pub struct PersistentSequenceCatalogInput {
    input: ImageDecoder<SequenceCatalog, StorageBackendError>,
}

impl PersistentSequenceCatalogInput {
    /// Reconstruct current durable records in a fresh private catalog while the caller retains the selected read transaction. This boundary never migrates metadata or changes autonomous sequence values.
    pub fn new(
        catalog: Arc<dyn CatalogFacade>,
        roles: RoleDefinitions,
        limits: CatalogInputLimits,
        mutations: &MemoryBudget,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        control.check()?;
        limits.validate()?;
        let retained = control.clone();
        let mutations = mutations.clone();
        let input = ImageDecoder::new(
            control.memory(),
            control.cancellation(),
            |mut reader| async move {
                let mut reader = reader.input();
                let control = ProductionControl::new(
                    retained.memory(),
                    retained.cancellation(),
                    reader.cancellation,
                );
                reader.checkpoint().await?;
                let mut source = catalog.begin_sequence_load(limits, &retained)?;
                let rows = loop {
                    reader.checkpoint().await?;
                    if let Some(rows) = source.advance(1)? {
                        break rows;
                    }
                };
                drop(source);
                let mut rows = Rows::<CatalogSequenceRow>::from(rows);
                let mut result = SequenceCatalog::with_budget(retained.memory());
                for row in rows.entries.by_ref() {
                    reader.checkpoint().await?;
                    rows::insert(
                        &mut result,
                        row.into_budgeted(),
                        &roles,
                        &control,
                        &mut reader,
                    )
                    .await?;
                }
                drop(rows);
                oids::read(
                    &mut result,
                    catalog.as_ref(),
                    limits,
                    &retained,
                    &mut reader,
                )
                .await?;
                retained.check()?;
                Ok(result.into_mutation_budget(&mutations))
            },
        )?;
        Ok(Self { input })
    }

    /// Physical slots, authority traversal, OID input and native publication share the original bounded work channel. Every terminal path consumes the unpublished generation.
    pub fn advance(&mut self, steps: usize) -> StorageBackendResult<Option<SequenceCatalog>> {
        self.input
            .advance(&mut std::io::empty(), ImageDecodeWindow { bytes: 1, steps })
    }
}

async fn copy_relation(
    relation: &RelationIdentity,
    input: &mut AsyncImageInput<'_>,
) -> StorageBackendResult<Budgeted<RelationIdentity>> {
    let (schema, mut memory) = input.copy_text(&relation.schema).await?.into_parts();
    let (name, name_memory) = input.copy_text(&relation.name).await?.into_parts();
    memory.absorb(name_memory);
    Ok(Budgeted::new(RelationIdentity { schema, name }, memory))
}

fn sql_error(error: SQLError) -> StorageBackendError {
    match error {
        SQLError::Cancelled(error) => error.into(),
        error => StorageBackendError::backend("sequence catalog", error),
    }
}

#[cfg(test)]
mod tests;
