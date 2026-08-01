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

use crate::block_max_index::{BlockMaxIndex, BlockMaxScorer, DEFAULT_BLOCK_SIZE};
use crate::inverted_index::{AnalyzerPhase, InvertedIndex};
use crate::sqlite::connection::{ManagedConnection, Result as SQLiteResult, SQLiteError};
use crate::StorageBackendResult;

#[derive(Clone)]
pub struct SQLiteInvertedIndex {
    conn: ManagedConnection,
    table: String,
    analyzer: Analyzer,
    index_field_analyzers: BTreeMap<FieldName, Analyzer>,
    search_field_analyzers: BTreeMap<FieldName, Analyzer>,
}

#[derive(Debug)]
struct StagedField {
    length: u64,
    postings: Vec<(String, Vec<u8>)>,
}

mod block_max;
mod codec;
mod core;
mod maintenance;
mod mutation;
mod trait_impl;

use codec::{
    blob_to_positions, corrupt_counter, decode_index_u64, decode_index_usize, encode_index_counter,
    encode_index_u64, encode_index_usize, invalidate_block_max_tables, load_document_lengths,
    load_field_total, positions_to_blob, quote_ident, table_exists, usize_to_index_u64,
    validate_position_count,
};

#[cfg(test)]
mod tests;
