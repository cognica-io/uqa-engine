//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Migration support for `SQLite` databases produced by the Python UQA
//! reference implementation.

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

use crate::{Engine, IVFIndexParams};

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
    #[error("destination exists but is not an empty uqa-rs catalog: {0}")]
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
    params: IVFIndexParams,
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

pub fn migrate_python_database(
    source: &Path,
    destination: &Path,
) -> Result<PythonMigrationReport, PythonMigrationError> {
    let source_path = resolve_source_database(source)?;
    reject_same_path(&source_path, destination)?;
    ensure_destination_empty(destination)?;

    let source_conn = open_read_only(&source_path)?;
    if !is_python_catalog(&source_conn)? {
        return Err(PythonMigrationError::NotPythonCatalog(source_path));
    }

    let index_rows = load_catalog_indexes(&source_conn)?;
    let specs = load_table_specs(&source_conn, &index_rows)?;
    let engine = Engine::open(destination)?;

    if !engine.table_names().is_empty() || !engine.list_graphs().is_empty() {
        return Err(PythonMigrationError::DestinationNotEmpty(
            destination.display().to_string(),
        ));
    }

    let mut report = PythonMigrationReport {
        source_path,
        destination_path: destination.to_path_buf(),
        tables: 0,
        documents: 0,
        fts_fields: 0,
        vector_fields: 0,
        indexes: 0,
        analyzers: 0,
        table_field_analyzers: 0,
        foreign_servers: 0,
        foreign_tables: 0,
        graphs: 0,
        graph_vertices: 0,
        graph_edges: 0,
        path_indexes: 0,
        scoring_params: 0,
        models: 0,
        column_stats: 0,
    };

    report.analyzers = migrate_analyzers(&source_conn, &engine)?;
    create_tables(&source_conn, &engine, &specs, &mut report)?;
    report.table_field_analyzers = migrate_table_field_analyzers(&source_conn, &engine)?;
    install_secondary_indexes(&engine, &specs, &mut report)?;
    migrate_documents(&source_conn, &engine, &specs, &mut report)?;
    report.indexes = persist_catalog_indexes(&engine, &index_rows);
    report.column_stats = migrate_column_stats(&source_conn, &engine)?;
    report.scoring_params = migrate_scoring_params(&source_conn, &engine)?;
    report.models = migrate_models(&source_conn, &engine)?;
    report.foreign_servers = migrate_foreign_servers(&source_conn, &engine)?;
    report.foreign_tables = migrate_foreign_tables(&source_conn, &engine)?;
    migrate_graphs(&source_conn, &engine, &mut report)?;
    report.path_indexes = migrate_path_indexes(&source_conn, &engine)?;

    Ok(report)
}

fn resolve_source_database(source: &Path) -> Result<PathBuf, PythonMigrationError> {
    if !source.exists() {
        return Err(PythonMigrationError::SourceMissing(source.to_path_buf()));
    }
    if source.is_file() {
        return Ok(source.to_path_buf());
    }
    let mut candidates = Vec::new();
    collect_sqlite_catalogs(source, &mut candidates)?;
    match candidates.len() {
        0 => Err(PythonMigrationError::SourceCatalogMissing(
            source.to_path_buf(),
        )),
        1 => Ok(candidates.remove(0)),
        _ => Err(PythonMigrationError::MultipleSourceCatalogs(
            candidates
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", "),
        )),
    }
}

fn collect_sqlite_catalogs(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), PythonMigrationError> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            collect_sqlite_catalogs(&path, out)?;
        } else if file_type.is_file()
            && has_sqlite_extension(&path)
            && is_python_catalog_file(&path)?
        {
            out.push(path);
        }
    }
    Ok(())
}

fn has_sqlite_extension(path: &Path) -> bool {
    let Some(ext) = path.extension().and_then(|s| s.to_str()) else {
        return false;
    };
    matches!(
        ext.to_ascii_lowercase().as_str(),
        "db" | "sqlite" | "sqlite3"
    )
}

fn is_python_catalog_file(path: &Path) -> Result<bool, PythonMigrationError> {
    let Ok(conn) = open_read_only(path) else {
        return Ok(false);
    };
    is_python_catalog(&conn).map_err(PythonMigrationError::from)
}

fn open_read_only(path: &Path) -> rusqlite::Result<Connection> {
    Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
}

fn reject_same_path(source: &Path, destination: &Path) -> Result<(), PythonMigrationError> {
    if destination.exists() {
        let src = fs::canonicalize(source)?;
        let dst = fs::canonicalize(destination)?;
        if src == dst {
            return Err(PythonMigrationError::SameSourceAndDestination(dst));
        }
    }
    Ok(())
}

fn ensure_destination_empty(destination: &Path) -> Result<(), PythonMigrationError> {
    if !destination.exists() || fs::metadata(destination)?.len() == 0 {
        return Ok(());
    }
    let conn = open_read_only(destination)?;
    let table_names = sqlite_table_names(&conn)?;
    if table_names.is_empty() {
        return Ok(());
    }
    if !table_names.iter().any(|name| name == "_tables") {
        return Err(PythonMigrationError::DestinationNotEmptyCatalog(
            destination.to_path_buf(),
        ));
    }
    let checked = [
        "_tables",
        "_documents",
        "_postings",
        "_vectors",
        "_named_graphs",
        "_graph_vertices",
        "_graph_edges",
        "_graph_membership",
        "_catalog_indexes",
        "_scoring_params",
        "_models",
        "_analyzers",
        "_foreign_servers",
        "_foreign_tables",
    ];
    let mut non_empty = Vec::new();
    for table in checked {
        if table_names.iter().any(|name| name == table) && row_count(&conn, table)? > 0 {
            non_empty.push(table.to_string());
        }
    }
    if non_empty.is_empty() {
        Ok(())
    } else {
        Err(PythonMigrationError::DestinationNotEmpty(
            non_empty.join(", "),
        ))
    }
}

fn sqlite_table_names(conn: &Connection) -> rusqlite::Result<Vec<String>> {
    let mut stmt =
        conn.prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

fn is_python_catalog(conn: &Connection) -> rusqlite::Result<bool> {
    table_exists(conn, "_catalog_tables")
}

fn table_exists(conn: &Connection, name: &str) -> rusqlite::Result<bool> {
    let exists: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1 LIMIT 1",
            [name],
            |row| row.get(0),
        )
        .optional()?;
    Ok(exists.is_some())
}

fn row_count(conn: &Connection, table: &str) -> rusqlite::Result<i64> {
    let sql = format!("SELECT COUNT(*) FROM {}", quote_ident(table));
    conn.query_row(&sql, [], |row| row.get(0))
}

fn load_catalog_indexes(conn: &Connection) -> Result<Vec<CatalogIndex>, PythonMigrationError> {
    if !table_exists(conn, "_catalog_indexes")? {
        return Ok(Vec::new());
    }
    let mut stmt = conn.prepare(
        "SELECT name, index_type, table_name, columns, parameters FROM _catalog_indexes",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (name, index_type, table_name, columns_json, parameters_json) = row?;
        out.push(CatalogIndex {
            name,
            index_type,
            table_name,
            columns: serde_json::from_str(&columns_json).unwrap_or_default(),
            parameters: parameters_to_string_map(&parameters_json),
        });
    }
    Ok(out)
}

fn load_table_specs(
    conn: &Connection,
    indexes: &[CatalogIndex],
) -> Result<Vec<TableSpec>, PythonMigrationError> {
    let mut stmt = conn.prepare("SELECT name, columns_json FROM _catalog_tables ORDER BY name")?;
    let rows = stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut specs = Vec::new();
    for row in rows {
        let (name, columns_json) = row?;
        let columns: Vec<PythonColumnDef> = serde_json::from_str(&columns_json)?;
        let rust_columns = columns
            .iter()
            .map(column_to_rust)
            .collect::<Result<Vec<_>, _>>()?;
        let fts_fields = infer_fts_fields(conn, &name, indexes)?;
        let vector_fields = infer_vector_fields(&name, &columns, indexes);
        specs.push(TableSpec {
            name,
            columns,
            rust_columns,
            fts_fields,
            vector_fields,
        });
    }
    Ok(specs)
}

fn infer_fts_fields(
    conn: &Connection,
    table: &str,
    indexes: &[CatalogIndex],
) -> Result<Vec<String>, PythonMigrationError> {
    let mut fields = BTreeSet::new();
    for idx in indexes
        .iter()
        .filter(|idx| idx.table_name == table && idx.index_type.eq_ignore_ascii_case("gin"))
    {
        for col in &idx.columns {
            fields.insert(col.clone());
        }
    }

    let prefix = format!("_inverted_{table}_");
    let mut stmt = conn.prepare(
        "SELECT name FROM sqlite_master WHERE type = 'table' AND name LIKE ?1 ORDER BY name",
    )?;
    let rows = stmt.query_map([format!("{prefix}%")], |row| row.get::<_, String>(0))?;
    for row in rows {
        let name = row?;
        if let Some(field) = name.strip_prefix(&prefix) {
            fields.insert(field.to_string());
        }
    }
    Ok(fields.into_iter().collect())
}

fn infer_vector_fields(
    table: &str,
    columns: &[PythonColumnDef],
    indexes: &[CatalogIndex],
) -> Vec<VectorSpec> {
    let mut params_by_field = BTreeMap::new();
    for idx in indexes.iter().filter(|idx| {
        idx.table_name == table
            && (idx.index_type.eq_ignore_ascii_case("ivf")
                || idx.index_type.eq_ignore_ascii_case("hnsw"))
    }) {
        for col in &idx.columns {
            params_by_field.insert(col.clone(), IVFIndexParams::from_map_lossy(&idx.parameters));
        }
    }

    let mut specs = Vec::new();
    for col in columns {
        let dimensions = match col.vector_dimensions {
            Some(dim) if dim > 0 => Some(dim),
            _ if col.type_name.eq_ignore_ascii_case("vector") => Some(0),
            _ => None,
        };
        if let Some(dimensions) = dimensions {
            specs.push(VectorSpec {
                field: col.name.clone(),
                dimensions,
                params: params_by_field
                    .remove(&col.name)
                    .unwrap_or_else(IVFIndexParams::default),
            });
        }
    }
    specs
}

fn column_to_rust(col: &PythonColumnDef) -> Result<ColumnDef, PythonMigrationError> {
    let ty = rust_column_type(col)?;
    Ok(ColumnDef {
        name: col.name.clone(),
        ty,
        primary_key: col.primary_key,
        not_null: col.not_null,
        auto_increment: col.auto_increment,
        unique: col.unique,
        default: col
            .default
            .as_ref()
            .filter(|value| !value.is_null())
            .map(json_to_value)
            .transpose()?
            .map(Expr::Literal),
        check: None,
        references: None,
    })
}

fn rust_column_type(col: &PythonColumnDef) -> Result<ColumnType, PythonMigrationError> {
    let raw = col.type_name.to_ascii_lowercase();
    if raw == "vector" {
        let Some(dim) = col.vector_dimensions else {
            return Err(PythonMigrationError::Invalid(format!(
                "VECTOR column {} is missing vector_dimensions",
                col.name
            )));
        };
        return Ok(ColumnType::Vector(dim));
    }
    if raw.ends_with("[]") || raw == "point" {
        return Ok(ColumnType::Json);
    }
    if let Some(ty) = python_temporal_type(&raw) {
        return Ok(ty);
    }
    if is_python_text_type(&raw) {
        return Ok(ColumnType::Text);
    }
    match raw.as_str() {
        "integer" | "int" | "int2" | "int4" | "int8" | "bigint" | "smallint" | "serial"
        | "bigserial" | "serial4" | "serial8" | "bool" | "boolean" => Ok(ColumnType::Integer),
        "real" | "float" | "float4" | "float8" | "double precision" => Ok(ColumnType::Real),
        "numeric" | "decimal" => Ok(ColumnType::Numeric {
            precision: col.numeric_precision,
            scale: col.numeric_scale.or(col.numeric_precision.map(|_| 0)),
        }),
        "json" | "jsonb" => Ok(ColumnType::Json),
        "bytea" => Ok(ColumnType::Bytea),
        _ => Ok(ColumnType::Text),
    }
}

fn is_python_text_type(raw: &str) -> bool {
    matches!(
        raw,
        "text" | "varchar" | "character varying" | "char" | "character" | "name" | "uuid"
    )
}

fn python_temporal_type(raw: &str) -> Option<ColumnType> {
    match raw {
        "date" => Some(ColumnType::Date),
        "time" | "time without time zone" => Some(ColumnType::Time),
        "timetz" | "time with time zone" => Some(ColumnType::TimeTz),
        "datetime" | "timestamp" | "timestamp without time zone" => Some(ColumnType::Timestamp),
        "timestamptz" | "timestamp with time zone" => Some(ColumnType::TimestampTz),
        _ => None,
    }
}

fn create_tables(
    _source: &Connection,
    engine: &Engine,
    specs: &[TableSpec],
    report: &mut PythonMigrationReport,
) -> Result<(), PythonMigrationError> {
    for spec in specs {
        engine.create_table(&spec.name, standard_analyzer("english"), Vec::new());
        for col in &spec.rust_columns {
            engine.register_column(&spec.name, col.clone());
        }
        for vector in &spec.vector_fields {
            if vector.dimensions == 0 {
                return Err(PythonMigrationError::Invalid(format!(
                    "VECTOR field {}.{} has unknown dimensions",
                    spec.name, vector.field
                )));
            }
            engine.rebuild_ivf_vector_field(
                &spec.name,
                vector.field.clone(),
                vector.dimensions,
                vector.params,
            );
        }
        report.tables += 1;
        report.vector_fields += spec.vector_fields.len();
    }
    Ok(())
}

fn migrate_analyzers(conn: &Connection, engine: &Engine) -> Result<usize, PythonMigrationError> {
    if !table_exists(conn, "_analyzers")? {
        return Ok(0);
    }
    let mut stmt = conn.prepare("SELECT name, config_json FROM _analyzers ORDER BY name")?;
    let rows = stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut count = 0;
    for row in rows {
        let (name, config_json) = row?;
        engine
            .register_named_analyzer(&name, &config_json)
            .map_err(PythonMigrationError::Invalid)?;
        count += 1;
    }
    Ok(count)
}

fn migrate_table_field_analyzers(
    conn: &Connection,
    engine: &Engine,
) -> Result<usize, PythonMigrationError> {
    if !table_exists(conn, "_table_field_analyzers")? {
        return Ok(0);
    }
    let mut stmt = conn.prepare(
        "SELECT table_name, field, phase, analyzer_name FROM _table_field_analyzers
         ORDER BY table_name, field, phase",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
        ))
    })?;
    let mut count = 0;
    for row in rows {
        let (table, field, phase, analyzer_name) = row?;
        engine
            .set_table_field_analyzer(&table, &field, &analyzer_name, &phase)
            .map_err(PythonMigrationError::Invalid)?;
        count += 1;
    }
    Ok(count)
}

fn install_secondary_indexes(
    engine: &Engine,
    specs: &[TableSpec],
    report: &mut PythonMigrationReport,
) -> Result<(), PythonMigrationError> {
    for spec in specs {
        for field in &spec.fts_fields {
            engine
                .add_fts_field(&spec.name, field.clone())
                .map_err(PythonMigrationError::Invalid)?;
            report.fts_fields += 1;
        }
    }
    Ok(())
}

fn migrate_documents(
    conn: &Connection,
    engine: &Engine,
    specs: &[TableSpec],
    report: &mut PythonMigrationReport,
) -> Result<(), PythonMigrationError> {
    for spec in specs {
        let mut rows = read_table_documents(conn, spec)?;
        if rows.is_empty() {
            rows = read_shared_documents(conn, spec)?;
        }
        let vector_fallbacks = read_vector_fallbacks(conn, spec)?;
        for (doc_id, mut document) in rows {
            let mut vectors = extract_vectors(&document, &spec.vector_fields);
            for vector in &spec.vector_fields {
                if vectors.contains_key(&vector.field) {
                    continue;
                }
                if let Some(value) = vector_fallbacks.get(&(doc_id, vector.field.clone())) {
                    vectors.insert(vector.field.clone(), value.clone());
                    document
                        .entry(vector.field.clone())
                        .or_insert_with(|| vector_value(value));
                }
            }
            engine.add_document_with_vectors(&spec.name, doc_id, document, vectors);
            report.documents += 1;
        }
    }
    Ok(())
}

fn read_table_documents(
    conn: &Connection,
    spec: &TableSpec,
) -> Result<Vec<MigratedDocument>, PythonMigrationError> {
    let table_name = format!("_data_{}", spec.name);
    if !table_exists(conn, &table_name)? {
        return Ok(Vec::new());
    }
    let select_cols = spec
        .columns
        .iter()
        .map(|col| quote_ident(&col.name))
        .collect::<Vec<_>>();
    let sql = if select_cols.is_empty() {
        format!("SELECT _rowid FROM {}", quote_ident(&table_name))
    } else {
        format!(
            "SELECT _rowid, {} FROM {} ORDER BY _rowid",
            select_cols.join(", "),
            quote_ident(&table_name)
        )
    };
    let mut stmt = conn.prepare(&sql)?;
    let mut cursor = stmt.query([])?;
    let mut out = Vec::new();
    while let Some(row) = cursor.next()? {
        let raw_id = row.get::<_, i64>(0)?;
        if raw_id < 0 {
            return Err(PythonMigrationError::Invalid(format!(
                "negative doc id {raw_id} in table {}",
                spec.name
            )));
        }
        let mut document = BTreeMap::new();
        for (idx, col) in spec.columns.iter().enumerate() {
            let raw = row.get_ref(idx + 1)?;
            if matches!(raw, ValueRef::Null) {
                continue;
            }
            let value = sqlite_value_to_uqa(raw, col)?;
            if !matches!(value, Value::Null) {
                document.insert(col.name.clone(), value);
            }
        }
        out.push((raw_id as DocId, document));
    }
    Ok(out)
}

fn read_shared_documents(
    conn: &Connection,
    spec: &TableSpec,
) -> Result<Vec<MigratedDocument>, PythonMigrationError> {
    if !table_exists(conn, "_documents")? {
        return Ok(Vec::new());
    }
    let cols = table_columns(conn, "_documents")?;
    let body_col = if cols.iter().any(|col| col == "data_json") {
        "data_json"
    } else if cols.iter().any(|col| col == "body") {
        "body"
    } else {
        return Ok(Vec::new());
    };
    let sql =
        format!("SELECT doc_id, {body_col} FROM _documents WHERE table_name = ?1 ORDER BY doc_id");
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([spec.name.as_str()], |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (raw_id, body) = row?;
        let json: serde_json::Value = serde_json::from_str(&body)?;
        let Value::Map(map) = json_to_value(&json)? else {
            return Err(PythonMigrationError::Invalid(format!(
                "document {raw_id} in table {} is not a JSON object",
                spec.name
            )));
        };
        out.push((raw_id as DocId, map));
    }
    Ok(out)
}

fn table_columns(conn: &Connection, table: &str) -> Result<Vec<String>, PythonMigrationError> {
    let sql = format!("PRAGMA table_info({})", quote_ident(table));
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(1))?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

fn sqlite_value_to_uqa(
    raw: ValueRef<'_>,
    col: &PythonColumnDef,
) -> Result<Value, PythonMigrationError> {
    let lower = col.type_name.to_ascii_lowercase();
    match raw {
        ValueRef::Null => Ok(Value::Null),
        ValueRef::Integer(n) if matches!(lower.as_str(), "bool" | "boolean") => {
            Ok(Value::Bool(n != 0))
        }
        ValueRef::Integer(n) if python_temporal_type(&lower).is_some() => {
            integer_to_temporal_value(n, &lower)
        }
        ValueRef::Integer(n) => Ok(Value::Int(n)),
        ValueRef::Real(n) => Ok(Value::Float(n)),
        ValueRef::Text(bytes) => {
            let text = std::str::from_utf8(bytes)
                .map_err(|e| PythonMigrationError::Invalid(format!("invalid text: {e}")))?;
            if let Some(value) = text_to_temporal_value(text, &lower)? {
                return Ok(value);
            }
            if lower == "json"
                || lower == "jsonb"
                || lower == "vector"
                || lower == "point"
                || lower.ends_with("[]")
            {
                match serde_json::from_str::<serde_json::Value>(text) {
                    Ok(json) => json_to_value(&json),
                    Err(_) => Ok(Value::Str(text.to_string())),
                }
            } else {
                Ok(Value::Str(text.to_string()))
            }
        }
        ValueRef::Blob(bytes) => {
            if lower == "vector" {
                Ok(vector_value(&blob_to_f32_vec(bytes)?))
            } else {
                Ok(Value::Bytes(bytes.to_vec()))
            }
        }
    }
}

fn integer_to_temporal_value(value: i64, raw_type: &str) -> Result<Value, PythonMigrationError> {
    let ty = python_temporal_type(raw_type)
        .ok_or_else(|| PythonMigrationError::Invalid(format!("not a temporal type: {raw_type}")))?;
    let temporal = match ty {
        ColumnType::Date => TemporalValue::Date {
            days: i32::try_from(value).map_err(|e| {
                PythonMigrationError::Invalid(format!("date day offset {value} out of range: {e}"))
            })?,
        },
        ColumnType::Time => TemporalValue::Time { micros: value },
        ColumnType::TimeTz => TemporalValue::TimeTz {
            micros: value,
            offset_minutes: 0,
        },
        ColumnType::Timestamp => TemporalValue::Timestamp { micros: value },
        ColumnType::TimestampTz => TemporalValue::TimestampTz { micros: value },
        _ => unreachable!(),
    };
    Ok(Value::Temporal(temporal))
}

fn text_to_temporal_value(
    text: &str,
    raw_type: &str,
) -> Result<Option<Value>, PythonMigrationError> {
    let Some(ty) = python_temporal_type(raw_type) else {
        return Ok(None);
    };
    let parsed = match ty {
        ColumnType::Date => TemporalValue::parse_date(text),
        ColumnType::Time => TemporalValue::parse_time(text),
        ColumnType::TimeTz => TemporalValue::parse_time_tz(text),
        ColumnType::Timestamp => TemporalValue::parse_timestamp(text),
        ColumnType::TimestampTz => TemporalValue::parse_timestamp_tz(text),
        _ => None,
    }
    .ok_or_else(|| {
        PythonMigrationError::Invalid(format!("invalid {raw_type} temporal value: {text}"))
    })?;
    Ok(Some(Value::Temporal(parsed)))
}

fn json_to_value(json: &serde_json::Value) -> Result<Value, PythonMigrationError> {
    match json {
        serde_json::Value::Null => Ok(Value::Null),
        serde_json::Value::Bool(v) => Ok(Value::Bool(*v)),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Ok(Value::Int(i))
            } else if let Some(f) = n.as_f64() {
                Ok(Value::Float(f))
            } else {
                Err(PythonMigrationError::Invalid(format!(
                    "unsupported JSON number {n}"
                )))
            }
        }
        serde_json::Value::String(s) => Ok(Value::Str(s.clone())),
        serde_json::Value::Array(items) => items
            .iter()
            .map(json_to_value)
            .collect::<Result<Vec<_>, _>>()
            .map(Value::List),
        serde_json::Value::Object(items) => {
            if let Ok(temporal) = serde_json::from_value::<TemporalValue>(json.clone()) {
                return Ok(Value::Temporal(temporal));
            }
            items
                .iter()
                .map(|(key, value)| Ok((key.clone(), json_to_value(value)?)))
                .collect::<Result<BTreeMap<_, _>, _>>()
                .map(Value::Map)
        }
    }
}

fn extract_vectors(
    document: &BTreeMap<String, Value>,
    specs: &[VectorSpec],
) -> BTreeMap<String, Vec<f32>> {
    let mut out = BTreeMap::new();
    for spec in specs {
        if let Some(value) = document.get(&spec.field).and_then(value_to_f32_vec) {
            out.insert(spec.field.clone(), value);
        }
    }
    out
}

fn value_to_f32_vec(value: &Value) -> Option<Vec<f32>> {
    match value {
        Value::List(items) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                match item {
                    Value::Int(n) => out.push(*n as f32),
                    Value::Float(n) => out.push(*n as f32),
                    _ => return None,
                }
            }
            Some(out)
        }
        Value::Bytes(bytes) => blob_to_f32_vec(bytes).ok(),
        _ => None,
    }
}

fn vector_value(vector: &[f32]) -> Value {
    Value::List(
        vector
            .iter()
            .map(|value| Value::Float(f64::from(*value)))
            .collect(),
    )
}

fn read_vector_fallbacks(
    conn: &Connection,
    spec: &TableSpec,
) -> Result<BTreeMap<(DocId, String), Vec<f32>>, PythonMigrationError> {
    let mut out = BTreeMap::new();
    for vector in &spec.vector_fields {
        let table_name = format!("_ivf_lists_{}_{}", spec.name, vector.field);
        if table_exists(conn, &table_name)? {
            let sql = format!(
                "SELECT doc_id, embedding FROM {} ORDER BY doc_id",
                quote_ident(&table_name)
            );
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt.query_map([], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?))
            })?;
            for row in rows {
                let (doc_id, blob) = row?;
                out.insert(
                    (doc_id as DocId, vector.field.clone()),
                    blob_to_f32_vec(&blob)?,
                );
            }
        }
    }
    Ok(out)
}

fn blob_to_f32_vec(blob: &[u8]) -> Result<Vec<f32>, PythonMigrationError> {
    if blob.len() % 4 != 0 {
        return Err(PythonMigrationError::Invalid(format!(
            "vector blob length {} is not divisible by 4",
            blob.len()
        )));
    }
    Ok(blob
        .chunks_exact(4)
        .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect())
}

fn persist_catalog_indexes(engine: &Engine, indexes: &[CatalogIndex]) -> usize {
    for idx in indexes {
        let options = idx
            .parameters
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect::<Vec<_>>();
        engine.register_catalog_index(
            &idx.name,
            &idx.index_type,
            &idx.table_name,
            &idx.columns,
            &options,
        );
    }
    indexes.len()
}

fn migrate_column_stats(conn: &Connection, engine: &Engine) -> Result<usize, PythonMigrationError> {
    if !table_exists(conn, "_column_stats")? {
        return Ok(0);
    }
    let Some(catalog) = engine.catalog.as_ref() else {
        return Ok(0);
    };
    let mut stmt = conn.prepare(
        "SELECT table_name, column_name, distinct_count, null_count,
                min_value, max_value, row_count, histogram, mcv_values, mcv_frequencies
           FROM _column_stats
          ORDER BY table_name, column_name",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, i64>(3)?,
            row.get::<_, Option<String>>(4)?,
            row.get::<_, Option<String>>(5)?,
            row.get::<_, i64>(6)?,
            row.get::<_, String>(7)?,
            row.get::<_, String>(8)?,
            row.get::<_, String>(9)?,
        ))
    })?;
    let mut count = 0;
    for row in rows {
        let (
            table_name,
            column_name,
            distinct_count,
            null_count,
            min_value,
            max_value,
            row_count,
            histogram_json,
            mcv_values_json,
            mcv_frequencies_json,
        ) = row?;
        catalog.save_column_stats(ColumnStatsInput {
            table_name: &table_name,
            column_name: &column_name,
            distinct_count,
            null_count,
            min_value: min_value.as_deref(),
            max_value: max_value.as_deref(),
            row_count,
            histogram_json: &histogram_json,
            mcv_values_json: &mcv_values_json,
            mcv_frequencies_json: &mcv_frequencies_json,
        })?;
        count += 1;
    }
    Ok(count)
}

fn migrate_scoring_params(
    conn: &Connection,
    engine: &Engine,
) -> Result<usize, PythonMigrationError> {
    if !table_exists(conn, "_scoring_params")? {
        return Ok(0);
    }
    let cols = table_columns(conn, "_scoring_params")?;
    let params_col = if cols.iter().any(|col| col == "params_json") {
        "params_json"
    } else {
        "params"
    };
    let sql = format!("SELECT name, {params_col} FROM _scoring_params ORDER BY name");
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut count = 0;
    for row in rows {
        let (name, params_json) = row?;
        engine.save_scoring_params(&name, &params_json)?;
        count += 1;
    }
    Ok(count)
}

fn migrate_models(conn: &Connection, engine: &Engine) -> Result<usize, PythonMigrationError> {
    if !table_exists(conn, "_models")? {
        return Ok(0);
    }
    let cols = table_columns(conn, "_models")?;
    let name_col = if cols.iter().any(|col| col == "model_name") {
        "model_name"
    } else {
        "name"
    };
    let body_col = if cols.iter().any(|col| col == "config_json") {
        "config_json"
    } else {
        "body"
    };
    let Some(catalog) = engine.catalog.as_ref() else {
        return Ok(0);
    };
    let sql = format!("SELECT {name_col}, {body_col} FROM _models ORDER BY {name_col}");
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut count = 0;
    for row in rows {
        let (name, body) = row?;
        catalog.save_model(&name, &body)?;
        count += 1;
    }
    Ok(count)
}

fn migrate_foreign_servers(
    conn: &Connection,
    engine: &Engine,
) -> Result<usize, PythonMigrationError> {
    if !table_exists(conn, "_foreign_servers")? {
        return Ok(0);
    }
    let mut stmt =
        conn.prepare("SELECT name, fdw_type, options FROM _foreign_servers ORDER BY name")?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    let mut count = 0;
    for row in rows {
        let (name, fdw_type, options_json) = row?;
        let options = json_object_to_pairs(&options_json);
        engine
            .register_foreign_server(name, fdw_type, options, true)
            .map_err(PythonMigrationError::Invalid)?;
        count += 1;
    }
    Ok(count)
}

fn migrate_foreign_tables(
    conn: &Connection,
    engine: &Engine,
) -> Result<usize, PythonMigrationError> {
    if !table_exists(conn, "_foreign_tables")? {
        return Ok(0);
    }
    let mut stmt = conn.prepare(
        "SELECT name, server_name, columns_json, options FROM _foreign_tables ORDER BY name",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
        ))
    })?;
    let mut count = 0;
    for row in rows {
        let (name, server_name, columns_json, options_json) = row?;
        let columns: Vec<PythonColumnDef> = serde_json::from_str(&columns_json)?;
        let rust_columns = columns
            .iter()
            .map(column_to_rust)
            .collect::<Result<Vec<_>, _>>()?;
        let options = json_object_to_pairs(&options_json);
        engine
            .register_foreign_table(name, server_name, rust_columns, options, true)
            .map_err(PythonMigrationError::Invalid)?;
        count += 1;
    }
    Ok(count)
}

fn migrate_graphs(
    conn: &Connection,
    engine: &Engine,
    report: &mut PythonMigrationReport,
) -> Result<(), PythonMigrationError> {
    let graph_names = load_graph_names(conn)?;
    for graph in &graph_names {
        engine.create_graph(graph);
    }
    report.graphs = graph_names.len();

    let vertices = load_vertices(conn)?;
    let edges = load_edges(conn)?;
    let memberships = load_graph_memberships(conn)?;

    for (entity_type, entity_id, graph_name) in memberships {
        match entity_type.as_str() {
            "vertex" => {
                if let Some(vertex) = vertices.get(&entity_id) {
                    engine.add_graph_vertex(vertex.clone(), &graph_name);
                }
            }
            "edge" => {
                if let Some(edge) = edges.get(&entity_id) {
                    engine.add_graph_edge(edge.clone(), &graph_name);
                }
            }
            _ => {}
        }
    }
    report.graph_vertices = vertices.len();
    report.graph_edges = edges.len();
    Ok(())
}

fn load_graph_names(conn: &Connection) -> Result<Vec<String>, PythonMigrationError> {
    let mut names = BTreeSet::new();
    for (table, column) in [("_named_graphs", "name"), ("_graph_catalog", "graph_name")] {
        if !table_exists(conn, table)? {
            continue;
        }
        let sql = format!("SELECT {column} FROM {table}");
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        for row in rows {
            names.insert(row?);
        }
    }
    Ok(names.into_iter().collect())
}

fn load_vertices(conn: &Connection) -> Result<BTreeMap<u64, Vertex>, PythonMigrationError> {
    if !table_exists(conn, "_graph_vertices")? {
        return Ok(BTreeMap::new());
    }
    let cols = table_columns(conn, "_graph_vertices")?;
    let has_label = cols.iter().any(|col| col == "label");
    let sql = if has_label {
        "SELECT vertex_id, label, properties_json FROM _graph_vertices ORDER BY vertex_id"
            .to_string()
    } else {
        "SELECT vertex_id, '' AS label, properties_json FROM _graph_vertices ORDER BY vertex_id"
            .to_string()
    };
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    let mut out = BTreeMap::new();
    for row in rows {
        let (id, label, props_json) = row?;
        let props = json_object_to_value_map(&props_json)?;
        out.insert(
            id as u64,
            Vertex {
                vertex_id: id as u64,
                label,
                properties: props,
            },
        );
    }
    Ok(out)
}

fn load_edges(conn: &Connection) -> Result<BTreeMap<u64, Edge>, PythonMigrationError> {
    if !table_exists(conn, "_graph_edges")? {
        return Ok(BTreeMap::new());
    }
    let mut stmt = conn.prepare(
        "SELECT edge_id, source_id, target_id, label, properties_json
           FROM _graph_edges
          ORDER BY edge_id",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
        ))
    })?;
    let mut out = BTreeMap::new();
    for row in rows {
        let (id, source_id, target_id, label, props_json) = row?;
        let props = json_object_to_value_map(&props_json)?;
        out.insert(
            id as u64,
            Edge {
                edge_id: id as u64,
                source_id: source_id as u64,
                target_id: target_id as u64,
                label,
                properties: props,
            },
        );
    }
    Ok(out)
}

fn load_graph_memberships(
    conn: &Connection,
) -> Result<Vec<(String, u64, String)>, PythonMigrationError> {
    if !table_exists(conn, "_graph_membership")? {
        return Ok(Vec::new());
    }
    let mut stmt = conn.prepare(
        "SELECT entity_type, entity_id, graph_name
           FROM _graph_membership
          ORDER BY graph_name, entity_type, entity_id",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (ty, id, graph) = row?;
        out.push((ty, id as u64, graph));
    }
    Ok(out)
}

fn migrate_path_indexes(conn: &Connection, engine: &Engine) -> Result<usize, PythonMigrationError> {
    if !table_exists(conn, "_path_indexes")? {
        return Ok(0);
    }
    let mut stmt =
        conn.prepare("SELECT graph_name, label_sequences FROM _path_indexes ORDER BY graph_name")?;
    let rows = stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut count = 0;
    for row in rows {
        let (graph_name, labels_json) = row?;
        let label_sequences: Vec<Vec<String>> = serde_json::from_str(&labels_json)?;
        if engine.has_graph(&graph_name) {
            engine.build_path_index("default", &graph_name, &label_sequences);
            count += 1;
        }
    }
    Ok(count)
}

fn json_object_to_value_map(json: &str) -> Result<BTreeMap<String, Value>, PythonMigrationError> {
    match json_to_value(&serde_json::from_str::<serde_json::Value>(json)?)? {
        Value::Map(map) => Ok(map),
        _ => Ok(BTreeMap::new()),
    }
}

fn json_object_to_pairs(json: &str) -> Vec<(String, String)> {
    parameters_to_string_map(json).into_iter().collect()
}

fn parameters_to_string_map(json: &str) -> BTreeMap<String, String> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(json) else {
        return BTreeMap::new();
    };
    let Some(map) = value.as_object() else {
        return BTreeMap::new();
    };
    map.iter()
        .map(|(key, value)| {
            let value = value
                .as_str()
                .map_or_else(|| value.to_string(), str::to_string);
            (key.clone(), value)
        })
        .collect()
}

fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}
