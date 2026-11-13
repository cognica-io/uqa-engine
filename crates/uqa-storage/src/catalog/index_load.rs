//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Complete index rows retain provider-specific physical or stable relation ordering.

use super::{
    input::{CatalogInput, CatalogInputStep},
    ordering::compare_names,
};
use crate::{
    read_control::StorageReadControl, CatalogIndexRow, StorageBackendError, StorageBackendResult,
};
use std::cmp::Ordering;
use uqa_core::{
    image_io::{AsyncImageInput, ImageDecodeWindow, ImageDecoder},
    memory::{Budgeted, BudgetedVec},
    ordering::{sort_by_with_progress, SortProgress},
};

#[derive(Clone, Copy)]
pub enum CatalogIndexOrder {
    Physical,
    Relation,
}

pub struct CatalogIndexRecord {
    row: Budgeted<CatalogIndexRow>,
    position: usize,
}

impl std::ops::Deref for CatalogIndexRecord {
    type Target = CatalogIndexRow;
    fn deref(&self) -> &Self::Target {
        &self.row
    }
}

impl CatalogIndexRecord {
    pub fn into_budgeted(self) -> Budgeted<CatalogIndexRow> {
        self.row
    }
}

pub type CatalogIndexes = BudgetedVec<CatalogIndexRecord>;

pub struct CatalogIndexLoad {
    input: ImageDecoder<CatalogIndexes, StorageBackendError>,
}

impl CatalogIndexLoad {
    /// Decode every original physical row before ordering or exposing definitions to semantic restoration. Rows retain their native buffers and construction allowances.
    pub fn new(
        mut source: CatalogInput<CatalogIndexRow>,
        order: CatalogIndexOrder,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        control.check()?;
        if !source.shares_context(control) {
            return Err(StorageBackendError::Other(
                "index catalog input and loader must share their original control".into(),
            ));
        }
        let retained = control.clone();
        let input = ImageDecoder::new(
            control.memory(),
            control.cancellation(),
            |mut reader| async move {
                let mut work = reader.input();
                let mut rows = CatalogIndexes::new(retained.memory());
                loop {
                    work.checkpoint().await?;
                    match source.advance()? {
                        CatalogInputStep::Pending => (),
                        CatalogInputStep::Row(row) => {
                            let position = rows.len();
                            rows.push(CatalogIndexRecord { row, position })?;
                        }
                        CatalogInputStep::Complete => break,
                    }
                }
                drop(source);
                if matches!(order, CatalogIndexOrder::Relation) {
                    sort_by_with_progress(&mut rows, &mut Relations(&mut work)).await?;
                }
                retained.check()?;
                Ok(rows)
            },
        )?;
        Ok(Self { input })
    }

    pub fn advance(&mut self, steps: usize) -> StorageBackendResult<Option<CatalogIndexes>> {
        self.input
            .advance(&mut std::io::empty(), ImageDecodeWindow { bytes: 1, steps })
    }
}

struct Relations<'a, 'c>(&'a mut AsyncImageInput<'c>);

impl SortProgress<CatalogIndexRecord, StorageBackendError> for Relations<'_, '_> {
    async fn checkpoint(&mut self) -> StorageBackendResult<()> {
        Ok(self.0.checkpoint().await?)
    }
    async fn compare(
        &mut self,
        left: &CatalogIndexRecord,
        right: &CatalogIndexRecord,
    ) -> StorageBackendResult<Ordering> {
        let schema = compare_names(&left.relation.schema, &right.relation.schema, self.0).await?;
        if schema != Ordering::Equal {
            return Ok(schema);
        }
        Ok(
            compare_names(&left.relation.name, &right.relation.name, self.0)
                .await?
                .then_with(|| left.position.cmp(&right.position)),
        )
    }
}

#[cfg(test)]
mod tests;
