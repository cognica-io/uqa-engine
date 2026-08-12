//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Python bindings for the UQA engine.

#![allow(clippy::needless_pass_by_value)]

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use pyo3::conversion::IntoPyObjectExt;
use pyo3::exceptions::{PyIOError, PyRuntimeError, PyTypeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyBool, PyBytes, PyDict, PyFloat, PyInt, PyIterator, PyList, PyString, PyTuple};
use uqa_core::{DecimalValue, TemporalValue, Value};
use uqa_engine::migration::{migrate_python_database, PythonMigrationReport};
use uqa_engine::{
    Engine, HybridSearchParams, RobustHybridSearchParams, SQLAggregateFunction, SQLAggregateState,
    SQLFunctionOptions, SQLFunctionVolatility, SQLParam, SQLResult, SQLScalarFunction,
    SQLTableFunction, SQLTableFunctionResult, ScoredEntry, ScoringMode,
};
use uqa_scoring::{BM25Params, CalibrationReport};
use uqa_sql::SQLError;
use uqa_storage::{DatabaseFileFormat, SQLiteCompressionOptions};

mod callbacks;
mod conversions;
mod engine;
mod errors;
mod inputs;
mod migration;
mod options;
mod output;
mod params;
mod result;
mod value_conversion;

use callbacks::{PyAggregateFunction, PyScalarFunction, PyTableFunction};
use conversions::{
    batch_from_py, ensure_callable, params_from_py, table_function_result_from_py,
    values_to_py_tuple,
};
use engine::{
    detect_database_file, open, open_auto, open_compressed, open_compressed_encrypted,
    open_encrypted,
};
use errors::{py_error_to_sql, runtime_error};
use inputs::{
    document_from_py, validate_binary_label, validate_binary_labels, validate_tensor,
    validate_vector, vector_values_from_py,
};
use migration::migrate_python_db;
use options::{compression_options, database_file_format_name, scoring_mode};
use output::{
    calibration_report_to_py, migration_report_to_py, parse_scoring_params, scored_entries_to_py,
};
use params::{scalar, tensor, vector, PySQLParam};
use result::PySQLResult;
use value_conversion::{
    float_map_from_py, float_map_to_py, map_to_py, rows_to_py, value_from_py, value_to_py,
};

#[pyclass(name = "Engine", module = "uqa._uqa")]
struct PyEngine {
    inner: Arc<Engine>,
}

#[pymodule]
#[pyo3(name = "_uqa")]
fn uqa_module(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyEngine>()?;
    m.add_class::<PySQLParam>()?;
    m.add_class::<PySQLResult>()?;
    m.add_function(wrap_pyfunction!(open, m)?)?;
    m.add_function(wrap_pyfunction!(open_encrypted, m)?)?;
    m.add_function(wrap_pyfunction!(open_auto, m)?)?;
    m.add_function(wrap_pyfunction!(detect_database_file, m)?)?;
    m.add_function(wrap_pyfunction!(open_compressed, m)?)?;
    m.add_function(wrap_pyfunction!(open_compressed_encrypted, m)?)?;
    m.add_function(wrap_pyfunction!(vector, m)?)?;
    m.add_function(wrap_pyfunction!(tensor, m)?)?;
    m.add_function(wrap_pyfunction!(scalar, m)?)?;
    m.add_function(wrap_pyfunction!(migrate_python_db, m)?)?;
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    Ok(())
}
