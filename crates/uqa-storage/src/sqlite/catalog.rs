//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Catalog: schema versioning, migrations, and persisted table metadata.
//!
//! The catalog owns a single `_metadata` table that records the schema version
//! plus a `_tables` table holding per-table analyzer config and the lists
//! of FTS / vector fields registered on each table. Concrete data tables
//! (`_documents`, `_document_blobs`, `_posting_clusters`,
//! `_posting_documents`, `_vectors`, ...) are created by catalog migrations
//! before their respective stores are exposed.

use rusqlite::{params, OptionalExtension};

use crate::backend::{StorageBackendError, StorageBackendResult};
use crate::catalog::{
    CatalogFacade, CatalogIndexRow, ColumnStatsInput, ColumnStatsRow, EdgeRow, ForeignTableRow,
    GraphSnapshot, RelationIdentity, RelationKind, SequenceOptions, SequenceReservationResult,
    SequenceRow, TableSchema, VectorFieldSchema, ViewRow,
};
use crate::sqlite::connection::{ManagedConnection, Result, SQLiteError};

use super::catalog_lifecycle::{
    columns_json_references, delete_table_rows_if_exists, drop_fts_aux_tables_for_field,
    drop_fts_aux_tables_for_table, quote_sql_identifier, rename_btree_field_rows_or_keep_existing,
    rename_field_rows_or_keep_existing, rename_fts_aux_tables_for_field, renamed_columns_json,
    table_exists, update_btree_table_name_rows_if_exists, update_table_name_rows_if_exists,
};

/// Bump this every time a migration is added.
pub const CURRENT_SCHEMA_VERSION: u32 = 31;

const LEGACY_VIEWS_METADATA_KEY: &str = "sql_views_json";
const LEGACY_SEQUENCES_METADATA_KEY: &str = "sql_sequences_json";

pub struct Catalog {
    conn: ManagedConnection,
    fts_storage_was_reset: bool,
}

mod analyzers;
mod facade;
mod foreign_indexes;
mod graph;
mod migration;
mod models_scoring;
mod schema_tables;
mod sequences_views;
mod stats;

use migration::{decode_catalog_id, encode_catalog_id, migration_relation};

#[cfg(test)]
mod tests;
