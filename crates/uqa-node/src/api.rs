//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Top-level Node-API factories and migration helpers.

use super::engine::Engine;
use super::input::runtime_error;
use super::results::{CompressionOptions, MigrationReport, SQLParam};
use super::{migrate_python_database, napi, Either, Float32Array, Path, Result};

// ---------------------------------------------------------------------
// Module functions
// ---------------------------------------------------------------------

#[napi]
pub fn open(path: String) -> Result<Engine> {
    Engine::open(path)
}

#[napi]
pub fn open_encrypted(path: String, key: String) -> Result<Engine> {
    Engine::open_encrypted(path, key)
}

#[napi]
pub fn open_auto(path: String, key: Option<String>) -> Result<Engine> {
    Engine::open_auto(path, key)
}

#[napi]
pub fn open_compressed(path: String, options: Option<CompressionOptions>) -> Result<Engine> {
    Engine::open_compressed(path, options)
}

#[napi]
pub fn open_compressed_encrypted(
    path: String,
    key: String,
    options: Option<CompressionOptions>,
) -> Result<Engine> {
    Engine::open_compressed_encrypted(path, key, options)
}

#[napi]
pub fn detect_database_file(path: String) -> Result<String> {
    Engine::detect_database_file(path)
}

#[napi]
pub fn vector(values: Either<Float32Array, Vec<f64>>) -> Result<SQLParam> {
    SQLParam::vector(values)
}

#[napi]
pub fn tensor(values: Vec<Vec<f64>>) -> Result<SQLParam> {
    SQLParam::tensor(values)
}

#[napi(js_name = "migratePythonDB")]
pub fn migrate_python_db(source: String, destination: String) -> Result<MigrationReport> {
    migrate_python_database(Path::new(&source), Path::new(&destination))
        .map_err(runtime_error)?
        .try_into()
}

// ---------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------
