//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Persistent and in-memory backing stores for UQA: documents, inverted
//! index, vector indexes (IVF), B-tree, in-memory spatial scan, block-max, and the
//! `SQLite` catalog.

pub mod backend;
pub mod block_max_index;
pub mod btree_index;
pub mod catalog;
pub mod document_store;
pub mod hnsw_index;
pub mod index_abc;
pub mod index_manager;
pub mod index_types;
pub mod inverted_index;
pub mod ivf_index;
pub mod key_value;
pub mod spatial_index;
pub mod sqlite;
pub mod transaction;
pub mod vector_index;

pub use backend::{
    PersistentStorageBackend, PersistentStorageProvider, PersistentStorageSession,
    SQLiteStorageBackend, SQLiteStorageProvider, StorageBackendError, StorageBackendResult,
};
pub use block_max_index::{BlockMaxIndex, BlockMaxScorer, DEFAULT_BLOCK_SIZE};
pub use btree_index::BTreeIndex;
pub use catalog::{
    CatalogFacade, CatalogIndexRow, ColumnStatsInput, ColumnStatsRow, EdgeRow, ForeignTableRow,
    GraphSnapshot, GraphVertexRow, RelationIdentity, RelationKind, SequenceRow, TableSchema,
    VectorFieldSchema, ViewRow,
};
pub use document_store::{DocumentStore, MemoryDocumentStore, SharedDocumentRow};
pub use hnsw_index::HNSWIndex;
pub use index_abc::Index;
pub use index_manager::{BTreeIndexHandle, IndexManager};
pub use index_types::{IndexDef, IndexType};
pub use inverted_index::{AnalyzerPhase, InvertedIndex, MemoryInvertedIndex};
pub use ivf_index::{IVFIndex, IVFState};
pub use key_value::{
    KeyValueBatch, KeyValueCatalog, KeyValueDocumentStore, KeyValueInvertedIndex,
    KeyValueStorageBackend, KeyValueStore, KeyValueVectorIndex, MemoryKeyValueStore,
};
pub use spatial_index::{haversine_distance, MemorySpatialIndex, SpatialIndex};
pub use sqlite::{
    detect_database_file_format, read_authenticated_anchor, Catalog, DatabaseFileFormat,
    ManagedConnection, SQLiteBTreeIndexStore, SQLiteCompressedContainerAnchor,
    SQLiteCompressionCodec, SQLiteCompressionOptions, SQLiteDocumentStore, SQLiteError,
    SQLiteHNSWIndex, SQLiteIVFIndex, SQLiteInvertedIndex, SQLiteVectorIndex,
};
pub use transaction::{
    InMemoryTransaction, SQLiteTransaction, Snapshotable, TransactionError, TxResult,
};
pub use vector_index::{
    cosine_similarity, HNSWIndexParams, IVFIndexParams, MemoryVectorIndex, VectorIndex,
    VectorIndexOpenMode, VectorIndexSpec,
};
