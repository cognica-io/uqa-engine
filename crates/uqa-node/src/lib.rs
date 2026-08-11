//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Node.js bindings for the UQA engine.
//!
//! Heavy operations (SQL, searches, calibration) are exposed as
//! Promise-returning methods executed on the libuv thread pool so the
//! JavaScript event loop never blocks; `*Sync` variants exist for
//! scripts and tests. User-defined SQL functions are not yet exposed:
//! calling back into JavaScript from engine execution threads needs a
//! threadsafe-function design that is planned separately.

// The Node-API value-conversion traits (`ToNapiValue` / `FromNapiValue`)
// are `unsafe fn`s over raw `napi_env` pointers; implementing them is
// the whole point of this crate, so the workspace-wide `unsafe_code`
// deny is relaxed here.
#![allow(unsafe_code)]
// napi-exported functions must take owned `String` arguments.
#![allow(clippy::needless_pass_by_value)]

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use napi::bindgen_prelude::*;
use napi_derive::napi;

use uqa_core::Value;
use uqa_engine::migration::{migrate_python_database, PythonMigrationReport};
use uqa_engine::{
    Engine as CoreEngine, HybridSearchParams, RobustHybridSearchParams, SQLParam as CoreSQLParam,
    SQLResult as CoreSQLResult, ScoredEntry, ScoringMode,
};
use uqa_scoring::{BM25Params, CalibrationReport as CoreCalibrationReport};
use uqa_storage::{DatabaseFileFormat, SQLiteCompressionOptions};

const MAX_SAFE_INTEGER: i64 = 9_007_199_254_740_991;

mod api;
mod engine;
mod input;
mod results;
mod tasks;
mod value;

pub use api::*;
pub use engine::Engine;
pub use results::{
    CalibrationReport, CompressionOptions, MigrationReport, ReliabilityBin, SQLNotice, SQLParam,
    SQLResult, SearchHit,
};
pub use tasks::{
    CalibrationReportTask, EstimateScoringParamsTask, HybridSearchTask, KNNSearchTask,
    LearnScoringParamsTask, RobustHybridSearchTask, RunCypherTask, SQLBatchTask, SQLTask,
    SearchTask, VectorSimilarityTask,
};
pub use value::JSValue;

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
