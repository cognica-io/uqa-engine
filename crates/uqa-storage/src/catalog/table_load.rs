//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Complete table catalogs retain physical input, original predicates and admitted replacements.

mod acl;
mod components;
pub use components::CatalogTableComponents;
mod ordering;
#[cfg(test)]
mod tests;

use super::{
    input::CatalogInputLimits,
    table_input::{CatalogTableInput, CatalogTableInputStep},
    TableSchema,
};
use crate::{
    key_value::KeyValueRead, read_control::StorageReadControl, StorageBackendError,
    StorageBackendResult,
};
use std::sync::Arc;
use uqa_core::{
    image_io::{AsyncImageInput, ImageDecodeWindow, ImageDecoder},
    memory::{Budgeted, BudgetedVec, MemoryReservation, OwnedSet},
};

/// The provider's original ordering boundary relative to independent ACL application.
#[derive(Clone, Copy)]
pub enum CatalogTableOrder {
    BeforeReplacements,
    AfterReplacements,
}

/// An original provider view and its table-owner predicate. The loader invokes it once per definition, then checks nonzero object identities for duplicates, before reading independent ACLs.
pub struct CatalogTableValidation {
    pub read: Arc<dyn KeyValueRead + Send + Sync>,
    pub validate: fn(
        &dyn KeyValueRead,
        &TableSchema,
        CatalogInputLimits,
        &StorageReadControl,
    ) -> StorageBackendResult<()>,
    pub limits: CatalogInputLimits,
}

/// A table and every externally admitted definition allocation. Field order retires the native table before returning its allowance; independently admitted revision nodes retain their own leases.
#[derive(Debug)]
pub struct CatalogTable {
    schema: Box<TableSchema>,
    memory: MemoryReservation,
}

impl std::ops::Deref for CatalogTable {
    type Target = TableSchema;

    fn deref(&self) -> &Self::Target {
        &self.schema
    }
}

impl CatalogTable {
    pub fn into_budgeted(self) -> Budgeted<Box<TableSchema>> {
        Budgeted::new(self.schema, self.memory)
    }
}

pub type CatalogTables = BudgetedVec<CatalogTable>;

/// A retained catalog assembly continuation. Each advance bounds record visits, ordering comparisons and ACL copy work; incomplete or failed assembly never publishes a partial catalog.
pub struct CatalogTableLoad {
    input: ImageDecoder<CatalogTables, StorageBackendError>,
}

impl CatalogTableLoad {
    /// Assemble under the input's original memory, cancellation and temporary-file context. A different context is rejected before reading the source or allocating a continuation.
    pub fn new(
        source: CatalogTableInput,
        order: CatalogTableOrder,
        validation: Option<CatalogTableValidation>,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        control.check()?;
        if !source.control().shares_context(control) {
            return Err(StorageBackendError::Other(
                "table catalog input and loader must share their original control".into(),
            ));
        }
        let owner = control.clone();
        let input = ImageDecoder::new(
            control.memory(),
            control.cancellation(),
            |mut reader| async move {
                assemble(source, order, validation, &owner, reader.input()).await
            },
        )?;
        Ok(Self { input })
    }

    /// Consume at most `steps` continuation polls. Encoded record sizes retain the independent limits selected when the physical input was created. Completion, error, cancellation or unwinding retires the original view; a retired loader cannot resume.
    pub fn advance(&mut self, steps: usize) -> StorageBackendResult<Option<CatalogTables>> {
        self.input
            .advance(&mut std::io::empty(), ImageDecodeWindow { bytes: 1, steps })
    }
}

async fn assemble(
    mut source: CatalogTableInput,
    order: CatalogTableOrder,
    validation: Option<CatalogTableValidation>,
    control: &StorageReadControl,
    mut progress: AsyncImageInput<'_>,
) -> StorageBackendResult<CatalogTables> {
    let mut tables = CatalogTables::new(control.memory());
    let mut replacements = acl::Records::new();
    loop {
        progress.checkpoint().await?;
        match source.advance()? {
            CatalogTableInputStep::Pending => (),
            CatalogTableInputStep::Definition(row) => {
                let (schema, memory) = row.into_parts();
                tables.push(CatalogTable { schema, memory })?;
            }
            CatalogTableInputStep::DefinitionsComplete => {
                validate(&tables, validation.as_ref(), control, &mut progress).await?;
                if matches!(order, CatalogTableOrder::BeforeReplacements) {
                    ordering::sort(&mut tables, &mut progress).await?;
                }
            }
            CatalogTableInputStep::AclReplacement(row) => replacements.insert(row, control)?,
            CatalogTableInputStep::Complete => break,
        }
    }
    drop(source);
    drop(validation);
    for table in tables.iter_mut() {
        replacements.apply(table, &mut progress).await?;
    }
    drop(replacements);
    if matches!(order, CatalogTableOrder::AfterReplacements) {
        ordering::sort(&mut tables, &mut progress).await?;
    }
    Ok(tables)
}

async fn validate(
    tables: &[CatalogTable],
    validation: Option<&CatalogTableValidation>,
    control: &StorageReadControl,
    progress: &mut AsyncImageInput<'_>,
) -> StorageBackendResult<()> {
    if let Some(validation) = validation {
        let mut objects = OwnedSet::new();
        for table in tables {
            progress.checkpoint().await?;
            (validation.validate)(validation.read.as_ref(), table, validation.limits, control)?;
            if table.object_id != [0; 16]
                && !objects.insert_budgeted(table.object_id, control.memory())?
            {
                return Err(StorageBackendError::Other(
                    "duplicate table object identity".into(),
                ));
            }
        }
    }
    Ok(())
}
