//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Complete schema decoding precedes the provider's original stable ordering boundary.

use super::SchemaRow;
use crate::{
    catalog::input::{CatalogInput, CatalogInputStep},
    read_control::StorageReadControl,
    StorageBackendError, StorageBackendResult,
};
use std::cmp::Ordering;
use uqa_core::{
    image_io::{AsyncImageInput, ImageDecodeWindow, ImageDecoder},
    memory::{Budgeted, BudgetedVec},
    ordering::{sort_by_with_progress, SortProgress},
};

#[derive(Clone, Copy)]
pub enum CatalogSchemaOrder {
    Physical,
    Name,
}

pub struct CatalogSchemaRow {
    row: Budgeted<SchemaRow>,
    position: usize,
}

impl std::ops::Deref for CatalogSchemaRow {
    type Target = SchemaRow;

    fn deref(&self) -> &Self::Target {
        &self.row
    }
}

impl CatalogSchemaRow {
    pub fn into_budgeted(self) -> Budgeted<SchemaRow> {
        self.row
    }
}

pub type CatalogSchemas = BudgetedVec<CatalogSchemaRow>;

pub struct CatalogSchemaLoad {
    input: ImageDecoder<CatalogSchemas, StorageBackendError>,
}

impl CatalogSchemaLoad {
    /// Retain one original row view. Decode every physical record before ordering or exposing any row to semantic restoration.
    pub fn new(
        mut source: CatalogInput<SchemaRow>,
        order: CatalogSchemaOrder,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        control.check()?;
        if !source.shares_context(control) {
            return Err(StorageBackendError::Other(
                "schema catalog input and loader must share their original control".into(),
            ));
        }
        let retained = control.clone();
        let input = ImageDecoder::new(
            control.memory(),
            control.cancellation(),
            |mut reader| async move {
                let mut reader = reader.input();
                let mut rows = CatalogSchemas::new(retained.memory());
                loop {
                    reader.checkpoint().await?;
                    match source.advance()? {
                        CatalogInputStep::Pending => (),
                        CatalogInputStep::Row(row) => {
                            rows.push(CatalogSchemaRow {
                                row,
                                position: rows.len(),
                            })?;
                        }
                        CatalogInputStep::Complete => break,
                    }
                }
                drop(source);
                if matches!(order, CatalogSchemaOrder::Name) {
                    sort_by_with_progress(&mut rows, &mut Progress(&mut reader)).await?;
                }
                retained.check()?;
                Ok(rows)
            },
        )?;
        Ok(Self { input })
    }

    /// Bound source slots and ordering work by native polls. Failure, cancellation, zero windows and unwinding destroy all unpublished rows and the original provider view.
    pub fn advance(&mut self, steps: usize) -> StorageBackendResult<Option<CatalogSchemas>> {
        self.input
            .advance(&mut std::io::empty(), ImageDecodeWindow { bytes: 1, steps })
    }
}

struct Progress<'a, 'b>(&'a mut AsyncImageInput<'b>);

impl SortProgress<CatalogSchemaRow, StorageBackendError> for Progress<'_, '_> {
    async fn checkpoint(&mut self) -> StorageBackendResult<()> {
        Ok(self.0.checkpoint().await?)
    }

    async fn compare(
        &mut self,
        left: &CatalogSchemaRow,
        right: &CatalogSchemaRow,
    ) -> StorageBackendResult<Ordering> {
        Ok(
            crate::catalog::ordering::compare_names(left.name(), right.name(), self.0)
                .await?
                .then_with(|| left.position.cmp(&right.position)),
        )
    }
}

#[cfg(test)]
mod tests;
