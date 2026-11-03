//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Complete physical images retain sealed record bytes without rebuilding graph or quantization artifacts.

use super::{invalid, DiskANNMemoryBuilder, DiskANNMemorySource, DiskANNRecordKey};
use crate::{
    diskann_index::format::{DiskANNManifest, DiskANNOriginEntry, DiskANNOriginLayout, PAGE_BYTES},
    image_io::{ImageInput, ImageOutput},
    StorageBackendResult,
};

impl DiskANNMemorySource {
    pub(in crate::diskann_index) fn visit_image_origins(
        &self,
        control: &crate::read_control::StorageReadControl,
        visit: &mut dyn FnMut(DiskANNOriginEntry) -> StorageBackendResult<()>,
    ) -> StorageBackendResult<()> {
        let manifest = self.seal.manifest();
        let summary = manifest
            .origins()
            .ok_or_else(|| invalid("memory image has no complete origin stream"))?;
        let layout = DiskANNOriginLayout::new(
            manifest.input().generation,
            manifest.input().dimensions,
            summary.documents(),
        )?;
        for (key, bytes) in &self.records.records {
            if let DiskANNRecordKey::Origins(first) = key {
                let batch = layout.decode(*first, bytes, control)?;
                for index in 0..batch.len() {
                    control.check()?;
                    visit(batch.entry(index).expect("validated origin batch"))?;
                }
            }
        }
        control.check()
    }

    pub(in crate::diskann_index) fn write_image(
        &self,
        output: &mut ImageOutput<'_>,
    ) -> StorageBackendResult<()> {
        output.count(self.records.records.len() - 1)?;
        for (key, bytes) in &self.records.records {
            let (tag, first) = match key {
                DiskANNRecordKey::Manifest => continue,
                DiskANNRecordKey::Codebook => (0, 0),
                DiskANNRecordKey::Codes(first) => (1, *first),
                DiskANNRecordKey::Side(first) => (2, *first),
                DiskANNRecordKey::Origins(first) => (3, *first),
            };
            output.bytes(&[tag])?;
            output.bytes(&first.to_le_bytes())?;
            output.sized(bytes)?;
        }
        output.count(self.records.graph.len())?;
        for (id, bytes) in &self.records.graph {
            output.bytes(&id.to_le_bytes())?;
            output.bytes(bytes)?;
        }
        output.control.check()
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
