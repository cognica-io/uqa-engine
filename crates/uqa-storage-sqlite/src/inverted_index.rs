//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQLite-backed inverted index.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use rusqlite::types::Value as SqlValue;
use rusqlite::{params, params_from_iter, OptionalExtension};
use uqa_analysis::Analyzer;
use uqa_core::{DocId, FieldName, IndexStats, Payload, PostingEntry, PostingList};

use crate::connection::{ManagedConnection, Result as SQLiteResult, SQLiteError};
use uqa_storage::block_max_index::{BlockMaxIndex, BlockMaxScorer, DEFAULT_BLOCK_SIZE};
use uqa_storage::clustered_postings::{
    ClusteredPostingCursor, EncodedScoreCluster, MaterializedPostingCursor, OccurrencePosting,
    PostingCursor,
};
use uqa_storage::inverted_index::{
    AnalyzerPhase, IndexedFieldMetadata, IndexedFieldRevision, InvertedIndex,
};
use uqa_storage::StorageBackendResult;
use uqa_storage::TokenTermKey;

#[derive(Clone)]
pub struct SQLiteInvertedIndex {
    conn: ManagedConnection,
    table: String,
    bindings: uqa_storage::inverted_index::AnalyzerBindings,
}

#[derive(Debug)]
struct StagedField {
    metadata: IndexedFieldMetadata,
    postings: BTreeMap<TokenTermKey, Vec<uqa_core::TokenOccurrence>>,
}

mod block_max;
mod clustered;
mod codec;
mod controlled;
mod core;
mod data;
mod format;
mod maintenance;
mod mutation;
mod queries;
mod trait_impl;

use clustered::{clustered_result, load_cluster, posting_cursor_from_rows, write_cluster};
use codec::{
    decode_index_u64, decode_index_usize, encode_index_counter, encode_index_u64,
    encode_index_usize, invalidate_posting_accelerators, load_document_lengths, quote_ident,
    table_exists,
};

#[cfg(test)]
mod tests;
