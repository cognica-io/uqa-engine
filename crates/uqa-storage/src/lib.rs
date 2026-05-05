//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Persistent and in-memory backing stores for UQA: documents, inverted
//! index, vector indexes (IVF), B-tree, `R*Tree`, block-max, and the
//! `SQLite` catalog.

pub mod block_max_index;
pub mod btree_index;
pub mod document_store;
pub mod index_abc;
pub mod index_manager;
pub mod index_types;
pub mod inverted_index;
pub mod ivf_index;
pub mod spatial_index;
pub mod sqlite;
pub mod transaction;
pub mod vector_index;

pub use block_max_index::{BlockMaxIndex, BlockMaxScorer, DEFAULT_BLOCK_SIZE};
pub use btree_index::BTreeIndex;
pub use document_store::{DocumentStore, MemoryDocumentStore};
pub use index_abc::Index;
pub use index_manager::{BTreeIndexHandle, IndexManager};
pub use index_types::{IndexDef, IndexType};
pub use inverted_index::{InvertedIndex, MemoryInvertedIndex};
pub use ivf_index::{IVFIndex, IVFState};
pub use spatial_index::{haversine_distance, MemorySpatialIndex, SpatialIndex};
pub use sqlite::{
    Catalog, ManagedConnection, SQLiteDocumentStore, SQLiteError, SQLiteInvertedIndex,
    SQLiteVectorIndex, TableSchema, VectorFieldSchema,
};
pub use transaction::{
    InMemoryTransaction, SQLiteTransaction, Snapshotable, TransactionError, TxResult,
};
pub use vector_index::{cosine_similarity, MemoryVectorIndex, VectorIndex};
