//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Memory `DiskANN` images retain canonical origins and sealed artifacts while restoring a fresh mutation lineage.

use super::source::Canonical;
use super::{Clock, DiskANNMemoryIndex, DiskANNMemoryOptions};
use crate::{
    diskann_index::{
        build::{DiskANNGenerationOptions, DiskANNTemporaryBudget},
        format::{
            DiskANNBuildProvenance, DiskANNGeneration, DiskANNManifest, DiskANNOriginSummary,
        },
        pages::{DiskANNMemorySource, DiskANNReadLimits},
        RetainedDiskANNIndex,
    },
    image_io::{ImageInput, ImageOutput, ImageRead, ImageReader},
    read_control::StorageReadControl,
    vector_index::image::{invalid, VectorImageSnapshot, VectorImageSource},
    StorageBackendResult,
};
use std::sync::Arc;

mod stream;

pub(super) fn reader<'a>(
    index: &'a RetainedDiskANNIndex<Canonical>,
    physical: &'a DiskANNMemorySource,
    options: DiskANNMemoryOptions,
    original: &'a StorageReadControl,
    control: &'a StorageReadControl,
) -> StorageBackendResult<ImageReader<'a>> {
    ImageReader::new(
        stream::DiskANNImageCursor::new(index, physical, options, original, control)?,
        control,
    )
}

pub(super) fn write(
    index: &RetainedDiskANNIndex<Canonical>,
    physical: &DiskANNMemorySource,
    options: DiskANNMemoryOptions,
    original: &StorageReadControl,
    writer: &mut dyn std::io::Write,
    control: &StorageReadControl,
) -> StorageBackendResult<u64> {
    let mut cursor = stream::DiskANNImageCursor::new(index, physical, options, original, control)?;
    while !cursor.prepare(128)?.ready {}
    crate::image_io::with_retained_control(writer, Some(original), |writer| {
        let mut out = ImageOutput {
            writer,
            control,
            written: 0,
        };
        let mut buffer = [0; 4096];
        loop {
            let count = cursor.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            out.bytes(&buffer[..count])?;
        }
        control.check()?;
        Ok(out.written)
    })
}

pub(crate) fn read(
    input: &mut ImageInput<'_>,
    dimensions: u32,
    temporary: &DiskANNTemporaryBudget,
) -> StorageBackendResult<DiskANNMemoryIndex> {
    let memory = input
        .control
        .memory()
        .reserve(size_of::<DiskANNMemoryIndex>())?;
    let generation = DiskANNGeneration::new(
        input.array()?,
        u64::from_le_bytes(input.array()?),
        u64::from_le_bytes(input.array()?),
        u64::from_le_bytes(input.array()?),
    )?;
    if generation.table() != 1 || generation.index() != 1 {
        return Err(invalid());
    }
    let bytes = input.bytes_with_limit(
        DiskANNManifest::ENCODED_BYTES
            + DiskANNBuildProvenance::ENCODED_BYTES
            + DiskANNOriginSummary::ENCODED_BYTES,
    )?;
    let manifest = DiskANNManifest::decode(generation, &bytes, input.control)?;
    drop(bytes);
    if manifest.input().dimensions != dimensions || manifest.origins().is_none() {
        return Err(invalid());
    }
    let options = read_options(input, &manifest)?;
    input.control.check_value_size(
        DiskANNManifest::ENCODED_BYTES + DiskANNBuildProvenance::ENCODED_BYTES,
        options.generation.max_record_bytes,
    )?;
    let physical =
        DiskANNMemorySource::read_image(input, &manifest, options.generation.max_record_bytes)?;
    // Decoded payloads keep their original admission. Future query and mutation
    // allocations belong to the independent destination, like an ordinary index.
    let owner = StorageReadControl::with_limit(
        crate::mvcc::VersionedSessionOptions::default().retained_bytes,
    );
    let canonical = Canonical::read_image(input, dimensions, generation, &physical, &owner)?;
    let index = RetainedDiskANNIndex::open_with_owner(
        canonical,
        Arc::new(physical.clone()),
        options.parameters,
        options.read,
        input.control,
        &owner,
    )?;
    // Existing origins and physical data identities are retained. New mutations and
    // generations belong to a fresh lineage, so the source and restored writer cannot
    // manufacture the same revision identity for divergent future values.
    let clock = Clock::fresh(input.control)?;
    let result = DiskANNMemoryIndex {
        index,
        physical,
        clock,
        options,
        temporary: temporary.clone(),
        control: owner,
        _memory: memory,
    };
    input.control.check()?;
    Ok(result)
}

fn read_options(
    input: &mut ImageInput<'_>,
    manifest: &DiskANNManifest,
) -> StorageBackendResult<DiskANNMemoryOptions> {
    let build = manifest.build_provenance().ok_or_else(invalid)?;
    Ok(DiskANNMemoryOptions {
        parameters: manifest.input().parameters,
        read: DiskANNReadLimits {
            resident_bytes: input.count()?,
            cache_bytes: input.count()?,
            max_in_flight_page_bytes: input.count()?,
            max_record_bytes: input.count()?,
        },
        partitions: build.partition_options(),
        merge: build.merge_options(),
        generation: DiskANNGenerationOptions {
            training: build.training(),
            code_batch_nodes: build.code_batch_nodes(),
            side_batch_entries: build.side_batch_entries(),
            max_record_bytes: input.count()?,
        },
    })
}

struct Captured {
    index: RetainedDiskANNIndex<Canonical>,
    physical: DiskANNMemorySource,
    options: DiskANNMemoryOptions,
    original: StorageReadControl,
}

impl VectorImageSource for Captured {
    fn image_reader<'a>(
        &'a self,
        control: &'a StorageReadControl,
    ) -> StorageBackendResult<ImageReader<'a>> {
        reader(
            &self.index,
            &self.physical,
            self.options,
            &self.original,
            control,
        )
    }

    fn write_image(
        &self,
        output: &mut dyn std::io::Write,
        control: &StorageReadControl,
    ) -> StorageBackendResult<u64> {
        write(
            &self.index,
            &self.physical,
            self.options,
            &self.original,
            output,
            control,
        )
    }
}

pub(super) fn capture(
    index: &DiskANNMemoryIndex,
    control: &StorageReadControl,
) -> StorageBackendResult<VectorImageSnapshot> {
    index.control.check()?;
    VectorImageSnapshot::new(
        Captured {
            index: index.index.clone(),
            physical: index.physical.clone(),
            options: index.options,
            original: index.control.clone(),
        },
        control,
    )
}
