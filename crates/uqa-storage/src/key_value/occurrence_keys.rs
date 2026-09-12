//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A table-owned namespace for complete occurrence indexes, separate from legacy scalar keys.

use super::codec::{
    other_error, push_segment, push_str, push_u64, read_segment, read_str, read_u64, single_str_key,
};
use super::{DocId, StorageBackendResult, TAG_OCCURRENCE_INDEX};
use crate::TokenTermKey;

pub(super) const SCORE: u8 = b's';
pub(super) const POSITIONS: u8 = b'p';
pub(super) const DOCUMENT: u8 = b'd';
pub(super) const LENGTH: u8 = b'l';
pub(super) const METADATA: u8 = b'm';
pub(super) const FIELD: u8 = b'f';
pub(super) const FORMAT: u8 = b'v';
pub(super) const FORMAT_NAME: &[u8] = b"occurrences-v2";

pub(super) fn table_prefix(table: &str) -> StorageBackendResult<Vec<u8>> {
    single_str_key(TAG_OCCURRENCE_INDEX, table)
}

pub(super) fn kind_prefix(table: &str, kind: u8) -> StorageBackendResult<Vec<u8>> {
    let mut key = table_prefix(table)?;
    key.push(kind);
    Ok(key)
}

pub(super) fn field_prefix(table: &str, kind: u8, field: &str) -> StorageBackendResult<Vec<u8>> {
    let mut key = kind_prefix(table, kind)?;
    push_str(&mut key, field)?;
    Ok(key)
}

pub(super) fn term_prefix(
    table: &str,
    kind: u8,
    field: &str,
    term: &TokenTermKey,
) -> StorageBackendResult<Vec<u8>> {
    let mut key = field_prefix(table, kind, field)?;
    push_segment(&mut key, term.as_bytes())?;
    Ok(key)
}

pub(super) fn cluster_key(
    table: &str,
    kind: u8,
    field: &str,
    term: &TokenTermKey,
    cluster: u64,
) -> StorageBackendResult<Vec<u8>> {
    let mut key = term_prefix(table, kind, field, term)?;
    push_u64(&mut key, cluster);
    Ok(key)
}

pub(super) fn document_prefix(
    table: &str,
    kind: u8,
    doc_id: DocId,
) -> StorageBackendResult<Vec<u8>> {
    let mut key = kind_prefix(table, kind)?;
    push_u64(&mut key, doc_id);
    Ok(key)
}

pub(super) fn document_key(
    table: &str,
    kind: u8,
    doc_id: DocId,
    field: &str,
) -> StorageBackendResult<Vec<u8>> {
    let mut key = document_prefix(table, kind, doc_id)?;
    push_str(&mut key, field)?;
    Ok(key)
}

pub(super) fn metadata_key(
    table: &str,
    field: &str,
    doc_id: DocId,
) -> StorageBackendResult<Vec<u8>> {
    let mut key = field_prefix(table, METADATA, field)?;
    push_u64(&mut key, doc_id);
    Ok(key)
}

fn header(key: &[u8], kind: u8) -> StorageBackendResult<usize> {
    if key.first() != Some(&TAG_OCCURRENCE_INDEX) {
        return Err(other_error("invalid occurrence index key"));
    }
    let mut offset = 1;
    read_str(key, &mut offset)?;
    if key.get(offset) != Some(&kind) {
        return Err(other_error("invalid occurrence key kind"));
    }
    Ok(offset + 1)
}

fn end(key: &[u8], offset: usize) -> StorageBackendResult<()> {
    if key.len() != offset {
        return Err(other_error("trailing bytes in occurrence index key"));
    }
    Ok(())
}

pub(super) fn read_cluster(
    key: &[u8],
    kind: u8,
) -> StorageBackendResult<(String, TokenTermKey, u64)> {
    let mut offset = header(key, kind)?;
    let field = read_str(key, &mut offset)?;
    let term = TokenTermKey::from_bytes(read_segment(key, &mut offset)?.to_vec())?;
    let cluster = read_u64(key, &mut offset)?;
    end(key, offset)?;
    Ok((field, term, cluster))
}

pub(super) fn read_document(key: &[u8], kind: u8) -> StorageBackendResult<(DocId, String)> {
    let mut offset = header(key, kind)?;
    let doc_id = read_u64(key, &mut offset)?;
    let field = read_str(key, &mut offset)?;
    end(key, offset)?;
    Ok((doc_id, field))
}

pub(super) fn read_field(key: &[u8]) -> StorageBackendResult<String> {
    let mut offset = header(key, FIELD)?;
    let field = read_str(key, &mut offset)?;
    end(key, offset)?;
    Ok(field)
}
