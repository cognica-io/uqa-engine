//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Persistent and in-memory backing stores for UQA: documents, inverted
//! index, vector indexes (IVF), B-tree, `R*Tree`, block-max, and the
//! `SQLite` catalog.

pub mod document_store;
pub mod inverted_index;

pub use document_store::{DocumentStore, MemoryDocumentStore};
pub use inverted_index::{InvertedIndex, MemoryInvertedIndex};
