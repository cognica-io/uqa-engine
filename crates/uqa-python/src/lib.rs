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
use pyo3::exceptions::{PyRuntimeError, PyTypeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyBool, PyBytes, PyDict, PyFloat, PyInt, PyIterator, PyList, PyString, PyTuple};
use uqa_core::{DecimalValue, TemporalValue, Value};
use uqa_engine::migration::{migrate_python_database, PythonMigrationReport};
use uqa_engine::{
    Engine, HybridSearchParams, SQLAggregateFunction, SQLAggregateState, SQLParam, SQLResult,
    SQLScalarFunction, SQLTableFunction, SQLTableFunctionResult, ScoredEntry, ScoringMode,
};
use uqa_scoring::{BM25Params, BayesianBM25Params};
use uqa_sql::SQLError;
use uqa_storage::SQLiteCompressionOptions;

struct PyScalarFunction {
    name: String,
    callable: Py<PyAny>,
}

impl SQLScalarFunction for PyScalarFunction {
    fn call(&self, args: &[Value]) -> Result<Value, SQLError> {
        Python::attach(|py| {
            let py_args = values_to_py_tuple(py, args)
                .map_err(|err| py_error_to_sql(py, &self.name, "prepare scalar args", err))?;
            let result = self
                .callable
                .bind(py)
                .call1(py_args)
                .map_err(|err| py_error_to_sql(py, &self.name, "call scalar function", err))?;
            value_from_py(&result)
                .map_err(|err| py_error_to_sql(py, &self.name, "convert scalar result", err))
        })
    }
}

struct PyTableFunction {
    name: String,
    callable: Py<PyAny>,
}

impl SQLTableFunction for PyTableFunction {
    fn call(&self, args: &[Value]) -> Result<SQLTableFunctionResult, SQLError> {
        Python::attach(|py| {
            let py_args = values_to_py_tuple(py, args)
                .map_err(|err| py_error_to_sql(py, &self.name, "prepare table args", err))?;
            let result = self
                .callable
                .bind(py)
                .call1(py_args)
                .map_err(|err| py_error_to_sql(py, &self.name, "call table function", err))?;
            table_function_result_from_py(&result)
                .map_err(|err| py_error_to_sql(py, &self.name, "convert table result", err))
        })
    }
}

struct PyAggregateFunction {
    name: String,
    factory: Py<PyAny>,
}

impl SQLAggregateFunction for PyAggregateFunction {
    fn create_state(&self) -> Box<dyn SQLAggregateState> {
        Python::attach(|py| match self.factory.bind(py).call0() {
            Ok(state) => Box::new(PyAggregateStateWrapper {
                name: self.name.clone(),
                state: Some(state.unbind()),
                init_error: None,
            }),
            Err(err) => Box::new(PyAggregateStateWrapper {
                name: self.name.clone(),
                state: None,
                init_error: Some(py_error_to_sql(
                    py,
                    &self.name,
                    "create aggregate state",
                    err,
                )),
            }),
        })
    }
}

struct PyAggregateStateWrapper {
    name: String,
    state: Option<Py<PyAny>>,
    init_error: Option<SQLError>,
}

impl SQLAggregateState for PyAggregateStateWrapper {
    fn observe(&mut self, args: &[Value]) -> Result<(), SQLError> {
        if let Some(err) = self.init_error.take() {
            return Err(err);
        }
        let Some(state) = self.state.as_ref() else {
            return Err(SQLError::Internal(format!(
                "Python aggregate `{}` has no state",
                self.name
            )));
        };
        Python::attach(|py| {
            let method = state
                .bind(py)
                .getattr("observe")
                .or_else(|_| state.bind(py).getattr("step"))
                .map_err(|err| py_error_to_sql(py, &self.name, "find aggregate observe", err))?;
            let py_args = values_to_py_tuple(py, args)
                .map_err(|err| py_error_to_sql(py, &self.name, "prepare aggregate args", err))?;
            method
                .call1(py_args)
                .map(|_| ())
                .map_err(|err| py_error_to_sql(py, &self.name, "observe aggregate value", err))
        })
    }

    fn finish(&self) -> Result<Value, SQLError> {
        if let Some(err) = self.init_error.as_ref() {
            return Err(SQLError::Internal(err.to_string()));
        }
        let Some(state) = self.state.as_ref() else {
            return Err(SQLError::Internal(format!(
                "Python aggregate `{}` has no state",
                self.name
            )));
        };
        Python::attach(|py| {
            let method = state
                .bind(py)
                .getattr("finish")
                .or_else(|_| state.bind(py).getattr("finalize"))
                .map_err(|err| py_error_to_sql(py, &self.name, "find aggregate finish", err))?;
            let result = method
                .call0()
                .map_err(|err| py_error_to_sql(py, &self.name, "finish aggregate", err))?;
            value_from_py(&result)
                .map_err(|err| py_error_to_sql(py, &self.name, "convert aggregate result", err))
        })
    }
}

#[pyclass(name = "SQLParam", module = "uqa._uqa", skip_from_py_object)]
#[derive(Clone)]
struct PySQLParam {
    inner: SQLParam,
}

#[pymethods]
impl PySQLParam {
    #[staticmethod]
    fn scalar(value: &Bound<'_, PyAny>) -> PyResult<Self> {
        Ok(Self {
            inner: SQLParam::scalar(value_from_py(value)?),
        })
    }

    #[staticmethod]
    fn vector(values: Vec<f32>) -> Self {
        Self {
            inner: SQLParam::vector(values),
        }
    }

    #[staticmethod]
    fn tensor(values: Vec<Vec<f32>>) -> Self {
        Self {
            inner: SQLParam::tensor(values),
        }
    }

    fn __repr__(&self) -> String {
        match &self.inner {
            SQLParam::Scalar(value) => format!("SQLParam.scalar({value:?})"),
            SQLParam::Vector(values) => format!("SQLParam.vector(len={})", values.len()),
            SQLParam::Tensor(values) => format!("SQLParam.tensor(rows={})", values.len()),
        }
    }
}

#[pyclass(name = "SQLResult", module = "uqa._uqa", skip_from_py_object)]
#[derive(Clone)]
struct PySQLResult {
    columns: Vec<String>,
    rows: Vec<BTreeMap<String, Value>>,
    affected_rows: u64,
}

impl From<SQLResult> for PySQLResult {
    fn from(result: SQLResult) -> Self {
        Self {
            columns: result.columns,
            rows: result.rows,
            affected_rows: result.affected_rows,
        }
    }
}

#[pymethods]
impl PySQLResult {
    #[getter]
    fn columns(&self) -> Vec<String> {
        self.columns.clone()
    }

    #[getter]
    fn affected_rows(&self) -> u64 {
        self.affected_rows
    }

    #[getter]
    fn rows(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        rows_to_py(py, &self.rows)
    }

    fn to_dict(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let dict = PyDict::new(py);
        dict.set_item("columns", self.columns.clone())?;
        dict.set_item("rows", rows_to_py(py, &self.rows)?)?;
        dict.set_item("affected_rows", self.affected_rows)?;
        Ok(dict.into_any().unbind())
    }

    fn __len__(&self) -> usize {
        self.rows.len()
    }

    fn __repr__(&self) -> String {
        format!(
            "SQLResult(columns={:?}, rows={}, affected_rows={})",
            self.columns,
            self.rows.len(),
            self.affected_rows
        )
    }
}

#[pyclass(name = "Engine", module = "uqa._uqa")]
struct PyEngine {
    inner: Arc<Engine>,
}

#[pymethods]
impl PyEngine {
    #[new]
    fn new() -> Self {
        Self {
            inner: Arc::new(Engine::new()),
        }
    }

    #[staticmethod]
    fn open(path: PathBuf) -> PyResult<Self> {
        Ok(Self {
            inner: Arc::new(Engine::open(&path).map_err(runtime_error)?),
        })
    }

    #[staticmethod]
    fn open_encrypted(path: PathBuf, key: &str) -> PyResult<Self> {
        Ok(Self {
            inner: Arc::new(Engine::open_encrypted(&path, key).map_err(runtime_error)?),
        })
    }

    #[staticmethod]
    #[pyo3(signature = (path, codec="zstd", page_size=None, chunk_pages=None, level=None))]
    fn open_compressed(
        path: PathBuf,
        codec: &str,
        page_size: Option<u32>,
        chunk_pages: Option<u32>,
        level: Option<i32>,
    ) -> PyResult<Self> {
        let compression = compression_options(codec, page_size, chunk_pages, level)?;
        Ok(Self {
            inner: Arc::new(Engine::open_compressed(&path, compression).map_err(runtime_error)?),
        })
    }

    #[staticmethod]
    #[pyo3(signature = (path, key, codec="zstd", page_size=None, chunk_pages=None, level=None))]
    fn open_compressed_encrypted(
        path: PathBuf,
        key: &str,
        codec: &str,
        page_size: Option<u32>,
        chunk_pages: Option<u32>,
        level: Option<i32>,
    ) -> PyResult<Self> {
        let compression = compression_options(codec, page_size, chunk_pages, level)?;
        Ok(Self {
            inner: Arc::new(
                Engine::open_compressed_encrypted(&path, key, compression)
                    .map_err(runtime_error)?,
            ),
        })
    }

    #[pyo3(signature = (query, params=None))]
    fn sql(
        &self,
        py: Python<'_>,
        query: &str,
        params: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<PySQLResult> {
        let query = query.to_string();
        let params = params_from_py(params)?;
        let result = py
            .detach(|| self.inner.sql(&query, &params))
            .map_err(runtime_error)?;
        Ok(result.into())
    }

    #[pyo3(signature = (query, params=None))]
    fn execute(
        &self,
        py: Python<'_>,
        query: &str,
        params: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<PySQLResult> {
        self.sql(py, query, params)
    }

    fn sql_batch(
        &self,
        py: Python<'_>,
        statements: &Bound<'_, PyAny>,
    ) -> PyResult<Vec<PySQLResult>> {
        let statements = batch_from_py(statements)?;
        let borrowed: Vec<(&str, &[SQLParam])> = statements
            .iter()
            .map(|(sql, params)| (sql.as_str(), params.as_slice()))
            .collect();
        let results = py
            .detach(|| self.inner.sql_batch(&borrowed))
            .map_err(runtime_error)?;
        Ok(results.into_iter().map(Into::into).collect())
    }

    fn register_scalar_function(
        &self,
        py: Python<'_>,
        name: &str,
        callable: Py<PyAny>,
    ) -> PyResult<()> {
        ensure_callable(py, &callable, "scalar function")?;
        self.inner
            .register_scalar_function(
                name,
                PyScalarFunction {
                    name: name.to_string(),
                    callable,
                },
            )
            .map_err(runtime_error)
    }

    fn register_table_function(
        &self,
        py: Python<'_>,
        name: &str,
        callable: Py<PyAny>,
    ) -> PyResult<()> {
        ensure_callable(py, &callable, "table function")?;
        self.inner
            .register_table_function(
                name,
                PyTableFunction {
                    name: name.to_string(),
                    callable,
                },
            )
            .map_err(runtime_error)
    }

    fn register_aggregate_function(
        &self,
        py: Python<'_>,
        name: &str,
        factory: Py<PyAny>,
    ) -> PyResult<()> {
        ensure_callable(py, &factory, "aggregate function factory")?;
        self.inner
            .register_aggregate_function(
                name,
                PyAggregateFunction {
                    name: name.to_string(),
                    factory,
                },
            )
            .map_err(runtime_error)
    }

    fn create_default_table(&self, name: &str, fts_fields: Vec<String>) {
        self.inner.create_default_table(name, fts_fields);
    }

    fn create_vector_field(&self, table: &str, field: &str, dimensions: u32) -> bool {
        self.inner.create_vector_field(table, field, dimensions)
    }

    fn add_document(&self, table: &str, doc_id: u64, document: &Bound<'_, PyAny>) -> PyResult<()> {
        self.inner
            .add_document(table, doc_id, document_from_py(document)?);
        Ok(())
    }

    fn add_document_with_vectors(
        &self,
        table: &str,
        doc_id: u64,
        document: &Bound<'_, PyAny>,
        vectors: &Bound<'_, PyAny>,
    ) -> PyResult<()> {
        self.inner.add_document_with_vector_values(
            table,
            doc_id,
            document_from_py(document)?,
            vector_values_from_py(vectors)?,
        );
        Ok(())
    }

    fn add_vector(&self, table: &str, doc_id: u64, field: &str, vector: Vec<f32>) -> bool {
        self.inner.add_vector(table, doc_id, field, vector)
    }

    fn add_vector_values(
        &self,
        table: &str,
        doc_id: u64,
        field: &str,
        vectors: Vec<Vec<f32>>,
    ) -> bool {
        self.inner.add_vector_values(table, doc_id, field, vectors)
    }

    fn get_document(&self, py: Python<'_>, table: &str, doc_id: u64) -> PyResult<Py<PyAny>> {
        match self.inner.get_document(table, doc_id) {
            Some(document) => map_to_py(py, &document),
            None => Ok(py.None()),
        }
    }

    fn delete_document(&self, table: &str, doc_id: u64) {
        self.inner.delete_document(table, doc_id);
    }

    fn document_count(&self, table: &str) -> u64 {
        self.inner.document_count(table)
    }

    #[pyo3(signature = (table, field, query, top_k=10, scoring="bm25"))]
    fn search(
        &self,
        py: Python<'_>,
        table: &str,
        field: &str,
        query: &str,
        top_k: usize,
        scoring: &str,
    ) -> PyResult<Py<PyAny>> {
        let mode = scoring_mode(scoring)?;
        let entries = py.detach(|| self.inner.search(table, field, query, &mode, top_k));
        scored_entries_to_py(py, &entries)
    }

    #[pyo3(signature = (table, field, vector, top_k=10))]
    fn knn_search(
        &self,
        py: Python<'_>,
        table: &str,
        field: &str,
        vector: Vec<f32>,
        top_k: usize,
    ) -> PyResult<Py<PyAny>> {
        let entries = py.detach(|| self.inner.knn_search(table, field, vector, top_k));
        scored_entries_to_py(py, &entries)
    }

    fn vector_similarity_search(
        &self,
        py: Python<'_>,
        table: &str,
        field: &str,
        vector: Vec<f32>,
        threshold: f32,
    ) -> PyResult<Py<PyAny>> {
        let entries = py.detach(|| {
            self.inner
                .vector_similarity_search(table, field, vector, threshold)
        });
        scored_entries_to_py(py, &entries)
    }

    #[pyo3(signature = (table, text_field, text_query, vector_field, query_vector, top_k=10, knn_pool=None, alpha=1.0))]
    #[allow(clippy::too_many_arguments)]
    fn hybrid_search(
        &self,
        py: Python<'_>,
        table: &str,
        text_field: &str,
        text_query: &str,
        vector_field: &str,
        query_vector: Vec<f32>,
        top_k: usize,
        knn_pool: Option<usize>,
        alpha: f64,
    ) -> PyResult<Py<PyAny>> {
        let params = HybridSearchParams {
            table,
            text_field,
            text_query,
            vector_field,
            query_vector,
            knn_pool: knn_pool.unwrap_or(top_k.saturating_mul(4).max(top_k)),
            alpha,
            top_k,
        };
        let entries = py.detach(|| self.inner.hybrid_search(&params));
        scored_entries_to_py(py, &entries)
    }

    #[pyo3(signature = (graph, query, params=None))]
    fn run_cypher(
        &self,
        py: Python<'_>,
        graph: &str,
        query: &str,
        params: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<PySQLResult> {
        let params = params
            .map(document_from_py)
            .transpose()?
            .unwrap_or_default();
        let (columns, rows) = py
            .detach(|| self.inner.run_cypher(graph, query, params))
            .map_err(runtime_error)?;
        Ok(PySQLResult {
            columns,
            rows,
            affected_rows: 0,
        })
    }

    fn create_graph(&self, name: &str) {
        self.inner.create_graph(name);
    }

    fn drop_graph(&self, name: &str) {
        self.inner.drop_graph(name);
    }

    fn list_graphs(&self) -> Vec<String> {
        self.inner.list_graphs()
    }

    fn list_path_indexes(&self) -> Vec<String> {
        self.inner.list_path_indexes()
    }

    fn table_names(&self) -> Vec<String> {
        self.inner.table_names()
    }

    fn list_views(&self) -> Vec<String> {
        self.inner.list_views()
    }

    fn list_schemas(&self) -> Vec<String> {
        self.inner.list_schemas()
    }

    fn list_sequences(&self) -> Vec<String> {
        self.inner.list_sequences()
    }

    fn list_named_analyzers(&self) -> Vec<String> {
        self.inner.list_named_analyzers()
    }

    fn list_foreign_servers(&self) -> Vec<String> {
        self.inner.list_foreign_servers()
    }

    fn list_foreign_tables(&self) -> Vec<String> {
        self.inner.list_foreign_tables()
    }

    fn cancel(&self) {
        self.inner.cancel();
    }

    fn close(&self) {
        self.inner.close();
    }

    fn __repr__(&self) -> String {
        format!("Engine(tables={:?})", self.inner.table_names())
    }
}

#[pyfunction]
fn open(path: PathBuf) -> PyResult<PyEngine> {
    PyEngine::open(path)
}

#[pyfunction]
fn open_encrypted(path: PathBuf, key: &str) -> PyResult<PyEngine> {
    PyEngine::open_encrypted(path, key)
}

#[pyfunction]
#[pyo3(signature = (path, codec="zstd", page_size=None, chunk_pages=None, level=None))]
fn open_compressed(
    path: PathBuf,
    codec: &str,
    page_size: Option<u32>,
    chunk_pages: Option<u32>,
    level: Option<i32>,
) -> PyResult<PyEngine> {
    PyEngine::open_compressed(path, codec, page_size, chunk_pages, level)
}

#[pyfunction]
#[pyo3(signature = (path, key, codec="zstd", page_size=None, chunk_pages=None, level=None))]
fn open_compressed_encrypted(
    path: PathBuf,
    key: &str,
    codec: &str,
    page_size: Option<u32>,
    chunk_pages: Option<u32>,
    level: Option<i32>,
) -> PyResult<PyEngine> {
    PyEngine::open_compressed_encrypted(path, key, codec, page_size, chunk_pages, level)
}

#[pyfunction]
fn vector(values: Vec<f32>) -> PySQLParam {
    PySQLParam::vector(values)
}

#[pyfunction]
fn tensor(values: Vec<Vec<f32>>) -> PySQLParam {
    PySQLParam::tensor(values)
}

#[pyfunction]
fn scalar(value: &Bound<'_, PyAny>) -> PyResult<PySQLParam> {
    PySQLParam::scalar(value)
}

#[pyfunction]
fn migrate_python_db(source: PathBuf, destination: PathBuf) -> PyResult<Py<PyAny>> {
    Python::attach(|py| {
        let report = migrate_python_database(&source, &destination).map_err(runtime_error)?;
        migration_report_to_py(py, &report)
    })
}

#[pymodule]
#[pyo3(name = "_uqa")]
fn uqa_module(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyEngine>()?;
    m.add_class::<PySQLParam>()?;
    m.add_class::<PySQLResult>()?;
    m.add_function(wrap_pyfunction!(open, m)?)?;
    m.add_function(wrap_pyfunction!(open_encrypted, m)?)?;
    m.add_function(wrap_pyfunction!(open_compressed, m)?)?;
    m.add_function(wrap_pyfunction!(open_compressed_encrypted, m)?)?;
    m.add_function(wrap_pyfunction!(vector, m)?)?;
    m.add_function(wrap_pyfunction!(tensor, m)?)?;
    m.add_function(wrap_pyfunction!(scalar, m)?)?;
    m.add_function(wrap_pyfunction!(migrate_python_db, m)?)?;
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    Ok(())
}

fn compression_options(
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

fn scoring_mode(scoring: &str) -> PyResult<ScoringMode> {
    match scoring.to_ascii_lowercase().as_str() {
        "bm25" => Ok(ScoringMode::BM25(BM25Params::default())),
        "bayesian" | "bayesian_bm25" => {
            Ok(ScoringMode::BayesianBM25(BayesianBM25Params::default()))
        }
        other => Err(PyValueError::new_err(format!(
            "unsupported scoring mode `{other}`"
        ))),
    }
}

fn params_from_py(params: Option<&Bound<'_, PyAny>>) -> PyResult<Vec<SQLParam>> {
    let Some(params) = params else {
        return Ok(Vec::new());
    };
    if params.is_none() {
        return Ok(Vec::new());
    }
    let iterator = PyIterator::from_object(params)?;
    iterator
        .map(|item| param_from_py(&item?))
        .collect::<PyResult<Vec<_>>>()
}

fn param_from_py(value: &Bound<'_, PyAny>) -> PyResult<SQLParam> {
    if let Ok(param) = value.extract::<PyRef<'_, PySQLParam>>() {
        return Ok(param.inner.clone());
    }
    Ok(SQLParam::scalar(value_from_py(value)?))
}

fn batch_from_py(statements: &Bound<'_, PyAny>) -> PyResult<Vec<(String, Vec<SQLParam>)>> {
    let iterator = PyIterator::from_object(statements)?;
    iterator
        .map(|item| {
            let item = item?;
            let tuple = item.cast::<pyo3::types::PyTuple>().map_err(|_| {
                PyTypeError::new_err("sql_batch entries must be (sql, params) tuples")
            })?;
            if tuple.len() != 2 {
                return Err(PyValueError::new_err(
                    "sql_batch entries must contain exactly two values",
                ));
            }
            let sql = tuple.get_item(0)?.extract::<String>()?;
            let params = params_from_py(Some(&tuple.get_item(1)?))?;
            Ok((sql, params))
        })
        .collect()
}

fn ensure_callable(py: Python<'_>, callable: &Py<PyAny>, label: &str) -> PyResult<()> {
    if callable.bind(py).is_callable() {
        Ok(())
    } else {
        Err(PyTypeError::new_err(format!("{label} must be callable")))
    }
}

fn values_to_py_tuple<'py>(py: Python<'py>, values: &[Value]) -> PyResult<Bound<'py, PyTuple>> {
    let items = values
        .iter()
        .map(|value| value_to_py(py, value))
        .collect::<PyResult<Vec<_>>>()?;
    PyTuple::new(py, items)
}

fn table_function_result_from_py(value: &Bound<'_, PyAny>) -> PyResult<SQLTableFunctionResult> {
    if let Ok(dict) = value.cast::<PyDict>() {
        let columns_obj = dict
            .get_item("columns")?
            .ok_or_else(|| PyValueError::new_err("table function dict result needs `columns`"))?;
        let rows_obj = dict
            .get_item("rows")?
            .ok_or_else(|| PyValueError::new_err("table function dict result needs `rows`"))?;
        let mut columns = columns_obj.extract::<Vec<String>>()?;
        let rows = table_rows_from_py(&rows_obj, &mut columns)?;
        return Ok(SQLTableFunctionResult { columns, rows });
    }

    if let Ok(tuple) = value.cast::<PyTuple>() {
        if tuple.len() == 2 {
            let mut columns = tuple.get_item(0)?.extract::<Vec<String>>()?;
            let rows_obj = tuple.get_item(1)?;
            let rows = table_rows_from_py(&rows_obj, &mut columns)?;
            return Ok(SQLTableFunctionResult { columns, rows });
        }
    }

    let mut columns = Vec::new();
    let rows = table_rows_from_py(value, &mut columns)?;
    Ok(SQLTableFunctionResult { columns, rows })
}

fn table_rows_from_py(
    rows_obj: &Bound<'_, PyAny>,
    columns: &mut Vec<String>,
) -> PyResult<Vec<Vec<Value>>> {
    let iterator = PyIterator::from_object(rows_obj)?;
    let mut rows = Vec::new();
    for row in iterator {
        let row = row?;
        if let Ok(dict) = row.cast::<PyDict>() {
            if columns.is_empty() {
                for (key, _) in dict.iter() {
                    columns.push(key.extract::<String>()?);
                }
            }
            let mut values = Vec::with_capacity(columns.len());
            for column in columns.iter() {
                match dict.get_item(column)? {
                    Some(value) => values.push(value_from_py(&value)?),
                    None => values.push(Value::Null),
                }
            }
            rows.push(values);
        } else {
            if columns.is_empty() {
                return Err(PyValueError::new_err(
                    "table function row sequences require explicit columns",
                ));
            }
            let values = PyIterator::from_object(&row)?
                .map(|item| value_from_py(&item?))
                .collect::<PyResult<Vec<_>>>()?;
            if values.len() != columns.len() {
                return Err(PyValueError::new_err(format!(
                    "table function row has {} values but {} columns",
                    values.len(),
                    columns.len()
                )));
            }
            rows.push(values);
        }
    }
    Ok(rows)
}

fn document_from_py(value: &Bound<'_, PyAny>) -> PyResult<BTreeMap<String, Value>> {
    let dict = value
        .cast::<PyDict>()
        .map_err(|_| PyTypeError::new_err("expected a dict"))?;
    let mut out = BTreeMap::new();
    for (key, value) in dict.iter() {
        out.insert(key.extract::<String>()?, value_from_py(&value)?);
    }
    Ok(out)
}

fn vector_values_from_py(value: &Bound<'_, PyAny>) -> PyResult<BTreeMap<String, Vec<Vec<f32>>>> {
    let dict = value
        .cast::<PyDict>()
        .map_err(|_| PyTypeError::new_err("expected a dict of vector fields"))?;
    let mut out = BTreeMap::new();
    for (key, value) in dict.iter() {
        let field = key.extract::<String>()?;
        let vectors = if let Ok(single) = value.extract::<Vec<f32>>() {
            vec![single]
        } else {
            value.extract::<Vec<Vec<f32>>>()?
        };
        out.insert(field, vectors);
    }
    Ok(out)
}

fn value_from_py(value: &Bound<'_, PyAny>) -> PyResult<Value> {
    if value.is_none() {
        return Ok(Value::Null);
    }
    if value.is_instance_of::<PyBool>() {
        return Ok(Value::Bool(value.extract()?));
    }
    if value.is_instance_of::<PyInt>() {
        return Ok(Value::Int(value.extract()?));
    }
    if value.is_instance_of::<PyFloat>() {
        return Ok(Value::Float(value.extract()?));
    }
    let decimal_type = value.py().import("decimal")?.getattr("Decimal")?;
    if value.is_instance(&decimal_type)? {
        let text = value.str()?.extract::<String>()?;
        return DecimalValue::parse(&text)
            .map(Value::Decimal)
            .ok_or_else(|| PyValueError::new_err(format!("invalid decimal value {text}")));
    }
    if value.is_instance_of::<PyString>() {
        return Ok(Value::Str(value.extract()?));
    }
    if let Ok(bytes) = value.cast::<PyBytes>() {
        return Ok(Value::Bytes(bytes.as_bytes().to_vec()));
    }
    if let Ok(dict) = value.cast::<PyDict>() {
        let mut out = BTreeMap::new();
        for (key, value) in dict.iter() {
            out.insert(key.extract::<String>()?, value_from_py(&value)?);
        }
        return Ok(Value::Map(out));
    }
    if let Ok(iterator) = PyIterator::from_object(value) {
        let values = iterator
            .map(|item| value_from_py(&item?))
            .collect::<PyResult<Vec<_>>>()?;
        return Ok(Value::List(values));
    }
    Err(PyTypeError::new_err(format!(
        "unsupported Python value type: {}",
        value.get_type().name()?
    )))
}

fn value_to_py(py: Python<'_>, value: &Value) -> PyResult<Py<PyAny>> {
    match value {
        Value::Null => Ok(py.None()),
        Value::Bool(value) => value.into_py_any(py),
        Value::Int(value) => value.into_py_any(py),
        Value::Float(value) => value.into_py_any(py),
        Value::Decimal(value) => decimal_to_py(py, value),
        Value::Str(value) => value.into_py_any(py),
        Value::Bytes(value) => Ok(PyBytes::new(py, value).into_any().unbind()),
        Value::Temporal(value) => temporal_to_string(value).into_py_any(py),
        Value::List(values) => {
            let list = PyList::empty(py);
            for value in values {
                list.append(value_to_py(py, value)?)?;
            }
            Ok(list.into_any().unbind())
        }
        Value::Map(values) => map_to_py(py, values),
    }
}

fn temporal_to_string(value: &TemporalValue) -> String {
    value.to_sql_string()
}

fn decimal_to_py(py: Python<'_>, value: &DecimalValue) -> PyResult<Py<PyAny>> {
    let decimal_type = py.import("decimal")?.getattr("Decimal")?;
    Ok(decimal_type.call1((value.to_sql_string(),))?.unbind())
}

fn map_to_py(py: Python<'_>, values: &BTreeMap<String, Value>) -> PyResult<Py<PyAny>> {
    let dict = PyDict::new(py);
    for (key, value) in values {
        dict.set_item(key, value_to_py(py, value)?)?;
    }
    Ok(dict.into_any().unbind())
}

fn rows_to_py(py: Python<'_>, rows: &[BTreeMap<String, Value>]) -> PyResult<Py<PyAny>> {
    let list = PyList::empty(py);
    for row in rows {
        list.append(map_to_py(py, row)?)?;
    }
    Ok(list.into_any().unbind())
}

fn scored_entries_to_py(py: Python<'_>, entries: &[ScoredEntry]) -> PyResult<Py<PyAny>> {
    let list = PyList::empty(py);
    for entry in entries {
        let dict = PyDict::new(py);
        dict.set_item("doc_id", entry.doc_id)?;
        dict.set_item("score", entry.score)?;
        list.append(dict)?;
    }
    Ok(list.into_any().unbind())
}

fn migration_report_to_py(py: Python<'_>, report: &PythonMigrationReport) -> PyResult<Py<PyAny>> {
    let dict = PyDict::new(py);
    dict.set_item("source_path", report.source_path.to_string_lossy().as_ref())?;
    dict.set_item(
        "destination_path",
        report.destination_path.to_string_lossy().as_ref(),
    )?;
    dict.set_item("tables", report.tables)?;
    dict.set_item("documents", report.documents)?;
    dict.set_item("fts_fields", report.fts_fields)?;
    dict.set_item("vector_fields", report.vector_fields)?;
    dict.set_item("indexes", report.indexes)?;
    dict.set_item("analyzers", report.analyzers)?;
    dict.set_item("table_field_analyzers", report.table_field_analyzers)?;
    dict.set_item("foreign_servers", report.foreign_servers)?;
    dict.set_item("foreign_tables", report.foreign_tables)?;
    dict.set_item("graphs", report.graphs)?;
    dict.set_item("graph_vertices", report.graph_vertices)?;
    dict.set_item("graph_edges", report.graph_edges)?;
    dict.set_item("path_indexes", report.path_indexes)?;
    dict.set_item("scoring_params", report.scoring_params)?;
    dict.set_item("models", report.models)?;
    dict.set_item("column_stats", report.column_stats)?;
    Ok(dict.into_any().unbind())
}

fn runtime_error(error: impl std::fmt::Display) -> PyErr {
    PyRuntimeError::new_err(error.to_string())
}

fn py_error_to_sql(py: Python<'_>, name: &str, action: &str, error: PyErr) -> SQLError {
    let err_type = error.get_type(py).name().map_or_else(
        |_| "PythonError".to_string(),
        |name| name.to_string_lossy().into_owned(),
    );
    SQLError::Internal(format!(
        "Python UDF `{name}` failed during {action}: {err_type}: {}",
        error.value(py)
    ))
}
