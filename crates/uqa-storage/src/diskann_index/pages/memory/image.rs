//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Complete physical images retain sealed record bytes without rebuilding graph or quantization artifacts.

use super::{invalid, DiskANNMemoryBuilder, DiskANNMemorySource, DiskANNRecordKey};
use crate::{
    diskann_index::format::{DiskANNManifest, DiskANNOriginBatch, DiskANNOriginLayout, PAGE_BYTES},
    image_io::ImageInput,
    read_control::StorageReadControl,
    StorageBackendResult,
};
use uqa_core::memory::{BudgetedMapIter, BudgetedVec};

mod stream;
pub(in crate::diskann_index) use stream::PhysicalImageCursor;

/// Sealed origin records have at most 64 entries; each call decodes one fixed-capacity batch.
pub(in crate::diskann_index) struct ImageOrigins<'a> {
    source: &'a DiskANNMemorySource,
    layout: DiskANNOriginLayout,
    first: u64,
    documents: u64,
}

impl<'a> ImageOrigins<'a> {
    pub(in crate::diskann_index) fn next_batch(
        &mut self,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<DiskANNOriginBatch<'a>>> {
        control.check()?;
        if self.first == self.documents {
            return Ok(None);
        }
        let bytes = self
            .source
            .records
            .records
            .get(&DiskANNRecordKey::Origins(self.first))
            .ok_or_else(|| invalid("missing image origin batch"))?;
        let batch = self.layout.decode(self.first, bytes, control)?;
        self.first += batch.len() as u64;
        Ok(Some(batch))
    }
}

impl DiskANNMemorySource {
    pub(in crate::diskann_index) fn image_origins(&self) -> StorageBackendResult<ImageOrigins<'_>> {
        let manifest = self.seal.manifest();
        let summary = manifest
            .origins()
            .ok_or_else(|| invalid("memory image has no complete origin stream"))?;
        let layout = DiskANNOriginLayout::new(
            manifest.input().generation,
            manifest.input().dimensions,
            summary.documents(),
        )?;
        Ok(ImageOrigins {
            source: self,
            layout,
            first: 0,
            documents: summary.documents(),
        })
    }

    pub(in crate::diskann_index) fn read_image(
        input: &mut ImageInput<'_>,
        manifest: &DiskANNManifest,
        maximum_record: usize,
    ) -> StorageBackendResult<Self> {
        let mut builder =
            DiskANNMemoryBuilder::new(manifest.input().generation, input.control.memory());
        let count = input.count()?;
        let mut previous = None;
        for _ in 0..count {
            let tag = input.array::<1>()?[0];
            let first = u64::from_le_bytes(input.array()?);
            let key = match (tag, first) {
                (0, 0) => DiskANNRecordKey::Codebook,
                (1, first) => DiskANNRecordKey::Codes(first),
                (2, first) => DiskANNRecordKey::Side(first),
                (3, first) => DiskANNRecordKey::Origins(first),
                _ => return Err(invalid("unknown image record key")),
            };
            if previous.is_some_and(|previous| previous >= key) {
                return Err(invalid("image records are not strictly ordered"));
            }
            let bytes = input.bytes_with_limit(maximum_record)?;
            builder.write_record(key, &bytes, input.control)?;
            previous = Some(key);
        }
        let count = u64::from_le_bytes(input.array()?);
        if count != manifest.layout().page_count() {
            return Err(invalid("image page count differs from manifest"));
        }
        let mut page = [0_u8; PAGE_BYTES];
        for expected in 0..count {
            let id = u64::from_le_bytes(input.array()?);
            if id != expected {
                return Err(invalid("image pages are not contiguous"));
            }
            input.exact(&mut page)?;
            builder.write_graph_page(id, &page, input.control)?;
        }
        builder.finish(*manifest, input.control)
    }
}
