//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQLite-backed exact and IVF vector indexes.

use std::sync::Arc;

use rusqlite::params;
use uqa_core::{DocId, Payload, PostingEntry, PostingList};

use crate::sqlite::connection::{ManagedConnection, Result as SQLiteResult, SQLiteError};
use crate::vector_index::{
    cosine_similarity, select_top_k_scored, validate_vector_values, VectorIndex,
};
use crate::StorageBackendResult;

mod brute_force;
mod codec;
mod hnsw;
mod ivf;

pub use brute_force::SQLiteVectorIndex;
pub use hnsw::SQLiteHNSWIndex;
pub use ivf::SQLiteIVFIndex;

use codec::{
    blob_to_vector, decode_doc_id, encode_doc_id, i64_to_usize, usize_to_u64,
    validate_persisted_ordinal_sequence, validate_vector_ordinal_count, vector_to_blob,
};

type EncodedVector = (i64, Vec<u8>);
type EncodedDocVectors = (i64, Vec<EncodedVector>);

#[cfg(test)]
mod tests;
