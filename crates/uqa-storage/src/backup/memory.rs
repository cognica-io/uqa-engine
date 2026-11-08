//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native memory image properties remain independent of SQL reconstruction.

mod address;
mod header;
#[cfg(any(windows, all(unix, not(target_os = "emscripten"))))]
mod import;
mod table;
pub use address::{
    memory_image_namespace, read_memory_image_address, DecodedMemoryImageAddress,
    DecodedMemoryTableUnit, MemoryImageFamily, MemoryImageKey, MemoryTableUnit,
};
pub use header::MemoryImageHeader;
#[cfg(any(windows, all(unix, not(target_os = "emscripten"))))]
pub use import::{MemoryImageImportReader, MemoryImageImportUnit};
pub use table::{
    memory_table_metadata_reader, read_memory_table_metadata, DecodedMemoryTableMetadata,
    MemoryRelationIdentities, MemoryTableMetadata, MemoryTableProperties, MemoryTableStatistics,
};

/// Required ordered catalog units in memory image version 1.
pub const MEMORY_IMAGE_CATALOG_COUNT: u16 = 24;
/// Required metadata units at the start of each table generation.
pub const MEMORY_IMAGE_TABLE_METADATA_COUNT: u8 = 10;
/// Named graph, model, path-index and analyzer resource families.
pub const MEMORY_IMAGE_RESOURCE_KINDS: u8 = 4;
