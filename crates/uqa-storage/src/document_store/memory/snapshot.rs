//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retained document generations admit their tracked footprint without walking the corpus.

use std::sync::Arc;

use uqa_core::memory::{MemoryBudget, MemoryReservation};

use super::{Document, MemoryDocumentState, MemoryDocumentStore, ProjectionCache};
use crate::{
    read_control::StorageReadControl, DocumentStore, ReadOnlySnapshot, StorageBackendError,
    StorageBackendResult,
};

#[derive(Debug, Clone)]
pub(super) struct Retention {
    memory: Arc<MemoryReservation>,
    pub(super) control: StorageReadControl,
}

pub(super) fn document_payload(document: &Document) -> StorageBackendResult<u128> {
    // This is the ordinary mutation owner, before publication. Only its new row is traversed;
    // capture and removal use the stored count and never walk values or dormant layouts.
    let scratch = MemoryBudget::new(usize::MAX);
    document.values().try_fold(0_u128, |total, value| {
        let bytes = value
            .retained_payload_bytes_with_check(&scratch, || Ok(()))
            .map_err(super::super::retained::retention_error)?;
        Ok(total + bytes as u128)
    })
}

impl Clone for MemoryDocumentState {
    fn clone(&self) -> Self {
        Self {
            documents: self.documents.clone(),
            layouts: self.layouts.clone(),
            row_payload_bytes: self.row_payload_bytes,
            retention: crate::retained_memory::RetainedMemory::default(),
            _image_memory: None,
        }
    }
}

impl MemoryDocumentState {
    pub(super) fn retained_bytes(&self) -> u128 {
        size_of::<Self>() as u128
            + self.row_payload_bytes
            + self.documents.retained_bytes()
            + self.layouts.retained_bytes()
    }
}

impl MemoryDocumentStore {
    pub(super) fn check_retained_read(&self) -> StorageBackendResult<()> {
        self.retention
            .as_ref()
            .map_or(Ok(()), |retention| retention.control.check())
    }

    pub(super) fn admit_write(&self) -> StorageBackendResult<()> {
        if self.retention.is_some() {
            return Err(StorageBackendError::Other(
                "cannot write a retained document snapshot".into(),
            ));
        }
        Ok(())
    }

    pub(super) fn controlled_snapshot(
        &self,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Arc<dyn DocumentStore>> {
        control.check()?;
        let control = self
            .retention
            .as_ref()
            .map_or(control, |retention| &retention.control);
        control.check()?;
        let memory = control.memory().reserve(size_of::<Self>())?;
        let build = |state_memory| -> StorageBackendResult<Arc<dyn DocumentStore>> {
            let snapshot: Arc<dyn DocumentStore> = Arc::new(Self {
                state: Arc::clone(&self.state),
                projections: ProjectionCache::default(),
                retention: Some(Retention {
                    memory: state_memory,
                    control: control.clone(),
                }),
            });
            control.check()?;
            Ok(Arc::new(
                ReadOnlySnapshot::with_retention(snapshot, memory)?
                    .with_document_read_control(control)?,
            ))
        };
        match &self.retention {
            Some(retention) => build(Arc::clone(&retention.memory)),
            None => self
                .state
                .retention
                .with_retained(self.state.retained_bytes(), control, build),
        }
    }
}

#[cfg(test)]
mod tests;
