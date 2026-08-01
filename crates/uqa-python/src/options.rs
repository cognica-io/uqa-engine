//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Compression, database-format, and scoring-mode option parsing.

use super::{
    runtime_error, BM25Params, DatabaseFileFormat, Engine, PyResult, PyValueError,
    SQLiteCompressionOptions, ScoringMode,
};

pub(super) fn compression_options(
    codec: &str,
    page_size: Option<u32>,
    chunk_pages: Option<u32>,
    level: Option<i32>,
) -> PyResult<SQLiteCompressionOptions> {
    let mut options = match codec.to_ascii_lowercase().as_str() {
        "zstd" => SQLiteCompressionOptions::zstd(),
        "lz4" => SQLiteCompressionOptions::lz4(),
        other => {
            return Err(PyValueError::new_err(format!(
                "unsupported compression codec `{other}`"
            )));
        }
    };
    if let Some(value) = page_size {
        options.page_size = value;
    }
    if let Some(value) = chunk_pages {
        options.chunk_pages = value;
    }
    if let Some(value) = level {
        options.level = value;
    }
    options.validate().map_err(PyValueError::new_err)
}

pub(super) fn scoring_mode(
    engine: &Engine,
    table: &str,
    field: &str,
    scoring: &str,
) -> PyResult<ScoringMode> {
    match scoring.to_ascii_lowercase().as_str() {
        "bm25" => Ok(ScoringMode::BM25(BM25Params::default())),
        "bayesian" | "bayesian_bm25" => Ok(ScoringMode::BayesianBM25(
            engine
                .bayesian_params_for(table, field)
                .map_err(runtime_error)?,
        )),
        other => Err(PyValueError::new_err(format!(
            "unsupported scoring mode `{other}`"
        ))),
    }
}

pub(super) fn database_file_format_name(format: DatabaseFileFormat) -> &'static str {
    match format {
        DatabaseFileFormat::Missing => "missing",
        DatabaseFileFormat::PlainSQLite => "sqlite",
        DatabaseFileFormat::CompressedContainer { encrypted: false } => "compressed",
        DatabaseFileFormat::CompressedContainer { encrypted: true } => "compressed_encrypted",
        DatabaseFileFormat::Unrecognized => "unrecognized",
    }
}
