//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQLite-backed [`DocumentStore`].

use std::collections::BTreeMap;
use std::sync::Arc;

use rusqlite::{params, OptionalExtension};
use uqa_core::{DecimalValue, DocId, TemporalValue, Value};

use crate::backend::StorageBackendResult;
use crate::document_store::{Document, DocumentStore};
use crate::sqlite::connection::{ManagedConnection, Result as SQLiteResult, SQLiteError};

const DOCUMENT_BLOBS_TABLE: &str = "_document_blobs";
const BLOB_MARKER_TYPE: &str = "$uqa_type";
const BLOB_MARKER_VALUE: &str = "document_blob";
const BLOB_MARKER_FIELD: &str = "field";
const BLOB_MARKER_ENCODING: &str = "encoding";
const VALUE_BLOB_MARKER_VALUE: &str = "value_blob";
const VALUE_BLOB_F64_LIST: &str = "f64_list";
const VALUE_BLOB_F64_TENSOR: &str = "f64_tensor";
const VALUE_BLOB_TYPED_JSON: &str = "typed_json_v1";
const MIN_NUMERIC_BLOB_VALUES: usize = 32;
const DOC_ID_IN_CHUNK: usize = 256;

type EncodedDocument = (Document, Vec<(String, Vec<u8>)>);

mod batching;
mod blob;
mod store;
mod trait_impl;
mod typed_value;

use batching::{
    allocation_error, chunk_bind_values, doc_id_in_placeholders, document_id_from_sqlite,
    read_doc_id, should_probe_doc_ids, sorted_unique_doc_ids, sqlite_doc_id,
};
use blob::{
    blob_marker, blob_marker_info, decode_json_field_value, delete_document_blob,
    hydrate_document_blobs, load_marked_document_blob, take_requested_field, upsert_document_blob,
    value_blob_marker,
};
use typed_value::{
    decode_legacy_document_body, decode_legacy_json_value, encode_document_blobs,
    encode_stored_value, StoredValue,
};

#[derive(Clone)]
pub struct SQLiteDocumentStore {
    conn: ManagedConnection,
    table: String,
}

#[cfg(test)]
mod tests;
