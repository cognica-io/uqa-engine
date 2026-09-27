//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Evaluated document selections compose raw sources without transferring physical provenance.

use super::{VectorRead, VectorReadSnapshot};
use crate::{mvcc::VersionError, read_control::StorageReadControl, StorageBackendResult};
use uqa_core::{
    memory::{Budgeted, BudgetedVec},
    DocId,
};

pub struct SelectedVectorRead {
    base: Option<VectorReadSnapshot>,
    selected: BudgetedVec<(DocId, Option<VectorReadSnapshot>)>,
    dimensions: u32,
    control: StorageReadControl,
}

impl SelectedVectorRead {
    /// Retain already evaluated replacements/deletions in strictly increasing document order. None for the base means the supplied selection is the entire visible field. Capture inspects only controls and dimensions.
    pub fn capture(
        base: Option<VectorReadSnapshot>,
        dimensions: u32,
        selected: impl IntoIterator<Item = (DocId, Option<VectorReadSnapshot>)>,
        control: &StorageReadControl,
    ) -> StorageBackendResult<VectorReadSnapshot> {
        control.check()?;
        let check = |source: &VectorReadSnapshot| {
            source.check_control(control)?;
            if source.dimensions() != dimensions {
                return Err(
                    VersionError::InvalidEncoding("selected vector source width mismatch")
                        .into_storage_error(),
                );
            }
            Ok(())
        };
        if let Some(base) = &base {
            check(base)?;
        }
        let mut sources = BudgetedVec::new(control.memory());
        let mut after = None;
        for (document, source) in selected {
            control.check()?;
            if after.is_some_and(|after| document <= after) {
                return Err(VersionError::InvalidEncoding(
                    "selected vector documents are not strictly increasing",
                )
                .into_storage_error());
            }
            if let Some(source) = &source {
                check(source)?;
            }
            sources.push((document, source))?;
            after = Some(document);
        }
        let source = Self {
            base,
            selected: sources,
            dimensions,
            control: control.clone(),
        };
        Ok(Budgeted::new(source, control.memory().empty_reservation()).into_shared()?)
    }

    fn source(&self, document: DocId) -> Option<&VectorReadSnapshot> {
        match self
            .selected
            .binary_search_by_key(&document, |entry| entry.0)
        {
            Ok(position) => self.selected[position].1.as_ref(),
            Err(_) => self.base.as_ref(),
        }
    }
}

impl VectorRead for SelectedVectorRead {
    fn check_control(&self, control: &StorageReadControl) -> StorageBackendResult<()> {
        self.control.check()?;
        control.check()?;
        if let Some(base) = &self.base {
            base.check_control(control)?;
        }
        Ok(())
    }
    fn dimensions(&self) -> u32 {
        self.dimensions
    }
    fn next_document_after(
        &self,
        mut after: Option<DocId>,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<DocId>> {
        loop {
            self.check_control(control)?;
            let base = self
                .base
                .as_ref()
                .map(|base| base.next_document_after(after, control))
                .transpose()?
                .flatten();
            let position = self
                .selected
                .partition_point(|entry| after.is_some_and(|after| entry.0 <= after));
            let selected = self.selected.get(position).map(|entry| entry.0);
            let Some(document) = base.into_iter().chain(selected).min() else {
                return Ok(None);
            };
            if after.is_some_and(|after| document <= after) {
                return Err(VersionError::InvalidEncoding(
                    "selected vector cursor did not advance",
                )
                .into_storage_error());
            }
            if self.document_vector_count(document, control)? != 0 {
                return Ok(Some(document));
            }
            after = Some(document);
        }
    }
    fn document_vector_count(
        &self,
        document: DocId,
        control: &StorageReadControl,
    ) -> StorageBackendResult<u64> {
        self.check_control(control)?;
        self.source(document).map_or(Ok(0), |source| {
            source.document_vector_count(document, control)
        })
    }
    fn read_vector(
        &self,
        document: DocId,
        ordinal: u32,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<BudgetedVec<f32>>> {
        self.check_control(control)?;
        self.source(document).map_or(Ok(None), |source| {
            source.read_vector(document, ordinal, control)
        })
    }
}
