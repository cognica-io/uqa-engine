//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Physical definitions and generation values finish before semantic sequence restoration.

use super::{
    input::{CatalogInput, CatalogInputLimits, CatalogInputStep, CatalogRowDecoder},
    SequenceRow, SequenceValuePosition,
};
use crate::{
    key_value::KeyValueRead, read_control::StorageReadControl, StorageBackendError,
    StorageBackendResult,
};
use std::{cmp::Ordering, sync::Arc};
use uqa_core::{
    image_io::{AsyncImageInput, ImageDecodeWindow, ImageDecoder},
    memory::{Budgeted, BudgetedVec},
    ordering::{sort_by_with_progress, SortProgress},
};

/// Physical definition plus the original optional in-definition value state.
pub struct CatalogSequenceDefinition {
    pub row: SequenceRow,
    pub legacy_position: Option<SequenceValuePosition>,
}

/// Read the selected generation from the same original provider view as its definition.
pub type CatalogSequencePosition = fn(
    &dyn KeyValueRead,
    &CatalogSequenceDefinition,
    CatalogInputLimits,
    &StorageReadControl,
) -> StorageBackendResult<SequenceValuePosition>;

pub struct CatalogSequenceRow {
    row: Budgeted<SequenceRow>,
    position: usize,
}

impl std::ops::Deref for CatalogSequenceRow {
    type Target = SequenceRow;
    fn deref(&self) -> &Self::Target {
        &self.row
    }
}

impl CatalogSequenceRow {
    pub fn into_budgeted(self) -> Budgeted<SequenceRow> {
        self.row
    }
}

pub type CatalogSequences = BudgetedVec<CatalogSequenceRow>;

pub struct CatalogSequenceLoad {
    input: ImageDecoder<CatalogSequences, StorageBackendError>,
}

impl CatalogSequenceLoad {
    /// Preserve all-definition decoding, then all-generation value resolution, then stable relation order. Both sources share one read owner by construction. Neither incomplete rows nor that owner escape on error.
    pub fn new(
        values: Arc<dyn KeyValueRead + Send + Sync>,
        prefix: &[u8],
        decode: CatalogRowDecoder<CatalogSequenceDefinition>,
        position: CatalogSequencePosition,
        limits: CatalogInputLimits,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        let mut definitions = CatalogInput::new(values.clone(), prefix, decode, limits, control)?;
        let retained = control.clone();
        let input = ImageDecoder::new(
            control.memory(),
            control.cancellation(),
            |mut reader| async move {
                let mut reader = reader.input();
                let mut decoded = BudgetedVec::new(retained.memory());
                loop {
                    reader.checkpoint().await?;
                    match definitions.advance()? {
                        CatalogInputStep::Pending => (),
                        CatalogInputStep::Row(row) => decoded.push(row)?,
                        CatalogInputStep::Complete => break,
                    }
                }
                drop(definitions);
                let (decoded, allocation) = decoded.into_parts();
                let mut rows = CatalogSequences::new(retained.memory());
                for definition in decoded {
                    reader.checkpoint().await?;
                    let value = position(values.as_ref(), &definition, limits, &retained)?;
                    let row = definition.try_map(|mut definition| {
                        definition.row.current = value.current;
                        definition.row.called = value.called;
                        definition.row.log_count = value.log_count;
                        Ok::<_, StorageBackendError>(definition.row)
                    })?;
                    rows.push(CatalogSequenceRow {
                        row,
                        position: rows.len(),
                    })?;
                }
                drop(allocation);
                drop(values);
                sort_by_with_progress(&mut rows, &mut Progress(&mut reader)).await?;
                retained.check()?;
                Ok(rows)
            },
        )?;
        Ok(Self { input })
    }

    /// Bound provider slots and sorting work by native polls; failure and zero windows consume the unpublished prefix.
    pub fn advance(&mut self, steps: usize) -> StorageBackendResult<Option<CatalogSequences>> {
        self.input
            .advance(&mut std::io::empty(), ImageDecodeWindow { bytes: 1, steps })
    }
}

struct Progress<'a, 'b>(&'a mut AsyncImageInput<'b>);
impl SortProgress<CatalogSequenceRow, StorageBackendError> for Progress<'_, '_> {
    async fn checkpoint(&mut self) -> StorageBackendResult<()> {
        Ok(self.0.checkpoint().await?)
    }
    async fn compare(
        &mut self,
        left: &CatalogSequenceRow,
        right: &CatalogSequenceRow,
    ) -> StorageBackendResult<Ordering> {
        let schema =
            super::ordering::compare_names(&left.relation.schema, &right.relation.schema, self.0)
                .await?;
        let name = if schema.is_eq() {
            super::ordering::compare_names(&left.relation.name, &right.relation.name, self.0)
                .await?
        } else {
            schema
        };
        Ok(name.then_with(|| left.position.cmp(&right.position)))
    }
}
