//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Group ordered canonical ordinals into one versioned origin per adopted document.

use super::invalid;
use crate::{
    diskann_index::format::{DiskANNCanonicalOrigin, DiskANNVectorVersion},
    StorageBackendError,
};
use uqa_core::DocId;

pub struct DiskANNAdoptedOrigins<F> {
    version: DiskANNVectorVersion,
    dimensions: u32,
    document: Option<DocId>,
    count: u64,
    stamp: F,
}

impl<F, E> DiskANNAdoptedOrigins<F>
where
    F: FnMut(DocId, DiskANNCanonicalOrigin) -> Result<(), E>,
    E: From<StorageBackendError>,
{
    pub fn new(version: DiskANNVectorVersion, dimensions: u32, stamp: F) -> Self {
        Self {
            version,
            dimensions,
            document: None,
            count: 0,
            stamp,
        }
    }

    pub fn push(&mut self, document: DocId, ordinal: u32) -> Result<(), E> {
        if self.document != Some(document) {
            self.stamp_document()?;
            self.document = Some(document);
            self.count = 0;
        }
        if u64::from(ordinal) != self.count {
            return Err(invalid("canonical ordinal sequence has a gap").into());
        }
        self.count += 1;
        Ok(())
    }

    pub fn finish(mut self) -> Result<(), E> {
        self.stamp_document()
    }

    fn stamp_document(&mut self) -> Result<(), E> {
        if let Some(document) = self.document {
            let origin = DiskANNCanonicalOrigin::new(self.version, self.dimensions, self.count)?;
            (self.stamp)(document, origin)?;
        }
        Ok(())
    }
}
