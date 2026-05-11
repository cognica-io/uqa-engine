//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQLite-backed persistence: connection, catalog, document store,
//! inverted index, vector index.

pub mod catalog;
pub mod compressed_vfs;
pub mod connection;
pub mod document_store;
pub mod inverted_index;
pub mod vector_index;

pub use catalog::{
    Catalog, CatalogIndexRow, ColumnStatsInput, ColumnStatsRow, EdgeRow, ForeignTableRow,
    TableSchema, VectorFieldSchema, CURRENT_SCHEMA_VERSION,
};
pub use compressed_vfs::{SQLiteCompressionCodec, SQLiteCompressionOptions};
pub use connection::{ManagedConnection, Result, SQLiteError};
pub use document_store::SQLiteDocumentStore;
pub use inverted_index::SQLiteInvertedIndex;
pub use vector_index::{SQLiteIVFIndex, SQLiteVectorIndex};
