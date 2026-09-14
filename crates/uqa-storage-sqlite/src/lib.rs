//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQLite-backed persistence: connection, catalog, document store,
//! inverted index, vector index.

pub mod btree_index;
pub mod catalog;
mod catalog_lifecycle;
pub mod compressed_vfs;
pub mod connection;
pub mod detect;
pub mod document_store;
pub mod inverted_index;
pub mod vector_index;

pub use btree_index::SQLiteBTreeIndexStore;
pub use catalog::{Catalog, CURRENT_SCHEMA_VERSION};
pub use compressed_vfs::{
    read_authenticated_anchor, SQLiteCompressedContainerAnchor, SQLiteCompressionCodec,
    SQLiteCompressionOptions,
};
pub use connection::{ManagedConnection, Result, SQLiteError};
pub use detect::{detect_database_file_format, DatabaseFileFormat};
pub use document_store::SQLiteDocumentStore;
pub use inverted_index::SQLiteInvertedIndex;
pub use uqa_storage::catalog::{
    CatalogFacade, CatalogIndexRow, ColumnStatsInput, ColumnStatsRow, EdgeRow, ForeignTableRow,
    TableSchema, VectorFieldSchema,
};
pub use vector_index::{SQLiteHNSWIndex, SQLiteIVFIndex, SQLiteVectorIndex};

pub mod backend;
pub mod block_max_index;
pub mod graph;
pub mod key_value;
mod read_control;
pub mod transaction;
mod value_index_key;

pub use backend::{SQLiteStorageBackend, SQLiteStorageProvider};
pub use block_max_index::SQLiteBlockMaxPersistence;
pub use graph::SQLiteGraphStore;
pub use key_value::{
    SQLiteKeyValueCatalog, SQLiteKeyValueStorage, SQLiteKeyValueStorageBackend, SQLiteKeyValueStore,
};
pub use transaction::SQLiteTransaction;
