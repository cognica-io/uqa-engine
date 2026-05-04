//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Persistent and in-memory backing stores for UQA: documents, inverted
//! index, vector indexes (IVF), B-tree, `R*Tree`, block-max, and the
//! `SQLite` catalog.

pub mod block_max_index;
pub mod document_store;
pub mod inverted_index;
pub mod sqlite;
pub mod vector_index;

pub use block_max_index::{BlockMaxIndex, BlockMaxScorer, DEFAULT_BLOCK_SIZE};
pub use document_store::{DocumentStore, MemoryDocumentStore};
pub use inverted_index::{InvertedIndex, MemoryInvertedIndex};
pub use sqlite::{
    Catalog, ManagedConnection, SQLiteDocumentStore, SQLiteInvertedIndex, SQLiteVectorIndex,
    SqliteError, TableSchema, VectorFieldSchema,
};
pub use vector_index::{cosine_similarity, MemoryVectorIndex, VectorIndex};
