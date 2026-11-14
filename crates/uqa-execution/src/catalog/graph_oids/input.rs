//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Physical metadata finishes before ordered native graph identity reconstruction.

use super::GRAPH_CATALOG_OIDS_METADATA_PREFIX;
use std::sync::Arc;
use uqa_core::{
    image_io::{ImageDecodeWindow, ImageDecoder},
    memory::{BudgetedVec, MemoryBudget, ProductionControl},
};
use uqa_sql::{catalog::graph_oids::GraphOidRegistry, SQLError};
use uqa_storage::{
    catalog::{input::CatalogInputLimits, text_input::CatalogTextStep},
    read_control::StorageReadControl,
    CatalogFacade, StorageBackendError, StorageBackendResult,
};

pub struct GraphOidCatalogInput {
    input: ImageDecoder<GraphOidRegistry, StorageBackendError>,
}

impl GraphOidCatalogInput {
    /// The caller owns the selected read transaction. No metadata is migrated and no missing OID is regenerated.
    pub fn new(
        catalog: Arc<dyn CatalogFacade>,
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
                let mut input = reader.input();
                let production = ProductionControl::new(
                    retained.memory(),
                    retained.cancellation(),
                    input.cancellation,
                );
                input.checkpoint().await?;
                let mut source = catalog.begin_metadata_input(
                    GRAPH_CATALOG_OIDS_METADATA_PREFIX,
                    limits,
                    &retained,
                )?;
                let mut rows = BudgetedVec::new(retained.memory());
                loop {
                    input.checkpoint().await?;
                    match source.advance()? {
                        CatalogTextStep::Pending => (),
                        CatalogTextStep::Row(row) => rows.push(row)?,
                        CatalogTextStep::Complete => break,
                    }
                }
                drop(source);
                let (rows, _memory) = rows.into_parts();
                let mut result = GraphOidRegistry::with_budget(retained.memory());
                for row in rows {
                    input.checkpoint().await?;
                    let graph = row
                        .name
                        .strip_prefix(GRAPH_CATALOG_OIDS_METADATA_PREFIX)
                        .unwrap_or(&row.name);
                    let name = input.copy_text(graph).await?;
                    let value = uqa_sql::catalog::graph_oids::input::decode(
                        &row.value,
                        &production,
                        &mut input,
                    )
                    .await
                    .map_err(decode_error)?;
                    result.insert_stored_entry(name, value).map_err(sql_error)?;
                }
                retained.check()?;
                Ok(result.into_mutation_budget(&mutations))
            },
        )?;
        Ok(Self { input })
    }

    /// Work windows include physical rows, JSON token fragments, nested entries and immutable publication. Errors and unwinding retire the complete unpublished root.
    pub fn advance(&mut self, steps: usize) -> StorageBackendResult<Option<GraphOidRegistry>> {
        self.input
            .advance(&mut std::io::empty(), ImageDecodeWindow { bytes: 1, steps })
    }
}

fn decode_error(error: SQLError) -> StorageBackendError {
    match error {
        SQLError::Internal(message) => {
            StorageBackendError::Serde(<serde_json::Error as serde::de::Error>::custom(message))
        }
        error => sql_error(error),
    }
}

fn sql_error(error: SQLError) -> StorageBackendError {
    match error {
        SQLError::Cancelled(error) => error.into(),
        error => StorageBackendError::backend("graph OIDs", error),
    }
}

#[cfg(test)]
mod tests;
