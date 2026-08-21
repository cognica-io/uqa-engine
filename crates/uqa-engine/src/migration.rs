//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Migration support for legacy UQA `SQLite` catalog directories.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::types::ValueRef;
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde::Deserialize;
use uqa_analysis::analyzer::standard_analyzer;
use uqa_core::{DocId, Edge, TemporalValue, Value, Vertex};
use uqa_sql::ast::{ColumnDef, ColumnType, Expr};
use uqa_storage::sqlite::ColumnStatsInput;

use crate::sql::convert_value_to_column_type;
use crate::{Engine, HNSWIndexParams, IVFIndexParams, VectorIndexSpec};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PythonMigrationReport {
    pub source_path: PathBuf,
    pub destination_path: PathBuf,
    pub tables: usize,
    pub documents: usize,
    pub fts_fields: usize,
    pub vector_fields: usize,
    pub indexes: usize,
    pub analyzers: usize,
    pub table_field_analyzers: usize,
    pub foreign_servers: usize,
    pub foreign_tables: usize,
    pub graphs: usize,
    pub graph_vertices: usize,
    pub graph_edges: usize,
    pub path_indexes: usize,
    pub scoring_params: usize,
    pub models: usize,
    pub column_stats: usize,
}

#[derive(Debug, thiserror::Error)]
pub enum PythonMigrationError {
    #[error("source path does not exist: {0}")]
    SourceMissing(PathBuf),
    #[error("source directory contains no Python UQA SQLite catalog: {0}")]
    SourceCatalogMissing(PathBuf),
    #[error("source directory contains multiple Python UQA SQLite catalogs: {0}")]
    MultipleSourceCatalogs(String),
    #[error("source is not a Python UQA SQLite catalog: {0}")]
    NotPythonCatalog(PathBuf),
    #[error("source and destination must be different paths: {0}")]
    SameSourceAndDestination(PathBuf),
    #[error("destination database is not empty: {0}")]
    DestinationNotEmpty(String),
    #[error("destination exists but is not an empty uqa-engine catalog: {0}")]
    DestinationNotEmptyCatalog(PathBuf),
    #[error("sqlite error: {0}")]
    SQLite(#[from] rusqlite::Error),
    #[error("storage error: {0}")]
    Storage(#[from] uqa_storage::SQLiteError),
    #[error("storage backend error: {0}")]
    StorageBackend(#[from] uqa_storage::StorageBackendError),
    #[error("SQL error: {0}")]
    SQL(#[from] uqa_sql::SQLError),
    #[error("JSON error: {0}")]
    JSON(#[from] serde_json::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("migration error: {0}")]
    Invalid(String),
}

#[derive(Debug, Clone)]
struct TableSpec {
    name: String,
    columns: Vec<PythonColumnDef>,
    rust_columns: Vec<ColumnDef>,
    fts_fields: Vec<String>,
    vector_fields: Vec<VectorSpec>,
}

#[derive(Debug, Clone)]
struct VectorSpec {
    field: String,
    dimensions: u32,
    index: VectorIndexSpec,
}

#[derive(Debug, Clone)]
struct CatalogIndex {
    name: String,
    index_type: String,
    table_name: String,
    columns: Vec<String>,
    parameters: BTreeMap<String, String>,
}

type MigratedDocument = (DocId, BTreeMap<String, Value>);

#[derive(Debug, Clone, Deserialize)]
#[allow(clippy::struct_excessive_bools)]
struct PythonColumnDef {
    name: String,
    type_name: String,
    #[serde(default)]
    primary_key: bool,
    #[serde(default)]
    not_null: bool,
    #[serde(default)]
    auto_increment: bool,
    #[serde(default)]
    default: Option<serde_json::Value>,
    #[serde(default)]
    vector_dimensions: Option<u32>,
    #[serde(default)]
    unique: bool,
    #[serde(default)]
    numeric_precision: Option<u32>,
    #[serde(default)]
    numeric_scale: Option<u32>,
}

mod documents;
mod graphs;
mod metadata;
mod run;
mod schema;
mod source;
mod table_setup;
mod values;
mod vectors;

pub use run::migrate_python_database;

use documents::migrate_documents;
use graphs::migrate_graphs;
use metadata::{
    migrate_column_stats, migrate_foreign_servers, migrate_foreign_tables, migrate_models,
    migrate_path_indexes, migrate_scoring_params, persist_catalog_indexes,
};
use schema::{column_to_rust, load_catalog_indexes, load_table_specs, python_temporal_type};
use source::{
    ensure_destination_empty, is_python_catalog, open_read_only, reject_same_path,
    resolve_source_database, table_exists,
};
use table_setup::{
    create_tables, install_secondary_indexes, migrate_analyzers, migrate_table_field_analyzers,
};
use values::{
    json_object_to_pairs, json_object_to_value_map, json_to_value, parameters_to_string_map,
    quote_ident, sqlite_value_to_uqa, table_columns,
};
use vectors::{blob_to_f32_vec, extract_vectors, read_vector_fallbacks, vector_value};

#[cfg(test)]
use documents::coerce_migrated_document;
#[cfg(test)]
use schema::{infer_vector_fields, rust_column_type};
#[cfg(test)]
use vectors::value_to_f32_vec;

#[cfg(test)]
mod tests;
