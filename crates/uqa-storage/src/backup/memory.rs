//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native memory image properties remain independent of SQL reconstruction.

mod address;
mod header;
mod table;
pub use address::{memory_image_namespace, MemoryImageFamily, MemoryImageKey, MemoryTableUnit};
pub use header::MemoryImageHeader;
pub use table::{
    memory_table_metadata_reader, read_memory_table_metadata, DecodedMemoryTableMetadata,
    MemoryRelationIdentities, MemoryTableMetadata, MemoryTableProperties, MemoryTableStatistics,
};
