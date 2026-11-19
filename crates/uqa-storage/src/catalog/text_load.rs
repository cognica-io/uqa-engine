//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Complete named text families retain the provider's original physical or stable name order.

use super::{
    ordering::compare_names,
    text_input::{CatalogTextInput, CatalogTextRow, CatalogTextStep},
};
use crate::{read_control::StorageReadControl, StorageBackendError, StorageBackendResult};
use std::cmp::Ordering;
use uqa_core::{
    image_io::{AsyncImageInput, ImageDecodeWindow, ImageDecoder},
    memory::{Budgeted, BudgetedVec},
    ordering::{sort_by_with_progress, SortProgress},
};

#[derive(Clone, Copy)]
pub enum CatalogTextOrder {
    Physical,
    Name,
}

pub struct CatalogTextRecord {
    row: Budgeted<CatalogTextRow>,
    position: usize,
}
impl std::ops::Deref for CatalogTextRecord {
    type Target = CatalogTextRow;
    fn deref(&self) -> &Self::Target {
        &self.row
    }
}
impl CatalogTextRecord {
    pub fn into_budgeted(self) -> Budgeted<CatalogTextRow> {
        self.row
    }
}
pub type CatalogTextRows = BudgetedVec<CatalogTextRecord>;

pub struct CatalogTextLoad {
    input: ImageDecoder<CatalogTextRows, StorageBackendError>,
}
impl CatalogTextLoad {
    /// Complete original physical decoding before result ordering. The retained source and load must share their original controls; records keep their original allocations.
    pub fn new(
        mut source: CatalogTextInput,
        order: CatalogTextOrder,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        control.check()?;
        if !source.shares_context(control) {
            return Err(StorageBackendError::Other(
                "catalog text input and loader must share their original control".into(),
            ));
        }
        let retained = control.clone();
        let input = ImageDecoder::new(
            control.memory(),
            control.cancellation(),
            |mut reader| async move {
                let mut work = reader.input();
                let mut rows = CatalogTextRows::new(retained.memory());
                loop {
                    work.checkpoint().await?;
                    match source.advance()? {
                        CatalogTextStep::Pending => (),
                        CatalogTextStep::Row(row) => {
                            let position = rows.len();
                            rows.push(CatalogTextRecord { row, position })?;
                        }
                        CatalogTextStep::Complete => break,
                    }
                }
                if matches!(order, CatalogTextOrder::Name) {
                    sort_by_with_progress(&mut rows, &mut Names(&mut work)).await?;
                }
                retained.check()?;
                Ok(rows)
            },
        )?;
        Ok(Self { input })
    }
    pub fn advance(&mut self, steps: usize) -> StorageBackendResult<Option<CatalogTextRows>> {
        self.input
            .advance(&mut std::io::empty(), ImageDecodeWindow { bytes: 1, steps })
    }
}
struct Names<'a, 'c>(&'a mut AsyncImageInput<'c>);
impl SortProgress<CatalogTextRecord, StorageBackendError> for Names<'_, '_> {
    async fn checkpoint(&mut self) -> StorageBackendResult<()> {
        self.0.checkpoint().await?;
        Ok(())
    }
    async fn compare(
        &mut self,
        left: &CatalogTextRecord,
        right: &CatalogTextRecord,
    ) -> StorageBackendResult<Ordering> {
        Ok(compare_names(&left.name, &right.name, self.0)
            .await?
            .then_with(|| left.position.cmp(&right.position)))
    }
}
