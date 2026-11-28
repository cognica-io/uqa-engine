//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Compression, database-format, and scoring-mode option parsing.

use super::{runtime_error, Engine, PyResult, PyValueError, SQLiteCompressionOptions, ScoringMode};

pub(super) fn compression_options(
    codec: &str,
    page_size: Option<u32>,
    chunk_pages: Option<u32>,
    level: Option<i32>,
) -> PyResult<SQLiteCompressionOptions> {
    SQLiteCompressionOptions::from_parts(
        codec.parse().map_err(PyValueError::new_err)?,
        page_size,
        chunk_pages,
        level,
    )
    .map_err(PyValueError::new_err)
}

pub(super) fn scoring_mode(
    engine: &Engine,
    table: &str,
    field: &str,
    scoring: &str,
) -> PyResult<ScoringMode> {
    engine
        .resolve_scoring_mode(
            table,
            field,
            scoring.parse().map_err(PyValueError::new_err)?,
        )
        .map_err(runtime_error)
}
