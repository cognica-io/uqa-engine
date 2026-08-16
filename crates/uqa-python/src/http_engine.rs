//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Python binding for authenticated local and Cloud HTTP SQL.

use std::future::Future;
use std::sync::{Arc, Mutex, OnceLock};

use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict};
use tokio::runtime::{Builder, Runtime};
use uqa_client::{HttpEngine, HttpEngineError, SQLStream, SQLStreamFrame, SecretString};
use uqa_sql::SQLParam;

use super::{batch_from_py, map_to_py, params_from_py, PySQLResult};

static HTTP_RUNTIME: OnceLock<Runtime> = OnceLock::new();

#[pyclass(name = "HttpEngine", module = "uqa._uqa")]
pub(super) struct PyHttpEngine {
    inner: Arc<HttpEngine>,
}

#[pyclass(name = "HttpSQLStream", module = "uqa._uqa")]
pub(super) struct PyHttpSQLStream {
    inner: Arc<Mutex<SQLStream>>,
    request_id: String,
}

#[pymethods]
impl PyHttpEngine {
    /// Connect to one local or Cloud UQA data-plane origin.
    #[new]
    fn new(url: &str, token: String) -> PyResult<Self> {
        Ok(Self {
            inner: Arc::new(
                HttpEngine::new(url, SecretString::from(token)).map_err(http_runtime_error)?,
            ),
        })
    }

    /// Read `UQA_URL` and `UQA_TOKEN` from the process environment.
    #[staticmethod]
    fn from_env() -> PyResult<Self> {
        Ok(Self {
            inner: Arc::new(HttpEngine::from_env().map_err(http_runtime_error)?),
        })
    }

    #[pyo3(signature = (query, params=None))]
    fn sql(
        &self,
        py: Python<'_>,
        query: &str,
        params: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<PySQLResult> {
        let inner = self.inner.clone();
        let query = query.to_owned();
        let params = params_from_py(params)?;
        let result = run_http(py, async move { inner.sql(&query, &params).await })?;
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

    #[pyo3(signature = (query, params=None))]
    fn sql_with_metadata(
        &self,
        py: Python<'_>,
        query: &str,
        params: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<(PySQLResult, String)> {
        let inner = self.inner.clone();
        let query = query.to_owned();
        let params = params_from_py(params)?;
        let execution = run_http(
            py,
            async move { inner.sql_with_metadata(&query, &params).await },
        )?;
        let request_id = execution.request_id().to_owned();
        Ok((execution.into_result().into(), request_id))
    }

    fn sql_batch(
        &self,
        py: Python<'_>,
        statements: &Bound<'_, PyAny>,
    ) -> PyResult<Vec<PySQLResult>> {
        let inner = self.inner.clone();
        let statements = batch_from_py(statements)?;
        let results = run_http(py, async move {
            let borrowed = borrowed_statements(&statements);
            inner.sql_batch(&borrowed).await
        })?;
        Ok(results.into_iter().map(Into::into).collect())
    }

    fn sql_batch_with_metadata(
        &self,
        py: Python<'_>,
        statements: &Bound<'_, PyAny>,
    ) -> PyResult<(Vec<PySQLResult>, String)> {
        let inner = self.inner.clone();
        let statements = batch_from_py(statements)?;
        let execution = run_http(py, async move {
            let borrowed = borrowed_statements(&statements);
            inner.sql_batch_with_metadata(&borrowed).await
        })?;
        let request_id = execution.request_id().to_owned();
        let results = execution
            .into_results()
            .into_iter()
            .map(Into::into)
            .collect();
        Ok((results, request_id))
    }

    #[pyo3(signature = (query, params=None))]
    fn sql_stream(
        &self,
        py: Python<'_>,
        query: &str,
        params: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<PyHttpSQLStream> {
        let inner = self.inner.clone();
        let query = query.to_owned();
        let params = params_from_py(params)?;
        let stream = run_http(py, async move { inner.sql_stream(&query, &params).await })?;
        let request_id = stream.request_id().to_owned();
        Ok(PyHttpSQLStream {
            inner: Arc::new(Mutex::new(stream)),
            request_id,
        })
    }

    fn __repr__(&self) -> String {
        format!("{:?}", self.inner)
    }
}

#[pymethods]
impl PyHttpSQLStream {
    #[getter]
    fn request_id(&self) -> &str {
        &self.request_id
    }

    fn next_frame(&self, py: Python<'_>) -> PyResult<Option<Py<PyAny>>> {
        let stream = self.inner.clone();
        let runtime = http_runtime()?;
        let frame = py.detach(move || {
            let mut stream = stream
                .lock()
                .map_err(|_| HttpEngineError::InvalidStreamSequence)?;
            runtime.block_on(stream.next_frame())
        });
        let frame = frame.map_err(http_runtime_error)?;
        frame.map(|frame| stream_frame_to_py(py, frame)).transpose()
    }

    fn __iter__(stream: PyRef<'_, Self>) -> PyRef<'_, Self> {
        stream
    }

    fn __next__(&self, py: Python<'_>) -> PyResult<Option<Py<PyAny>>> {
        self.next_frame(py)
    }

    fn __repr__(&self) -> String {
        format!("HttpSQLStream(request_id={:?})", self.request_id)
    }
}

fn borrowed_statements(statements: &[(String, Vec<SQLParam>)]) -> Vec<(&str, &[SQLParam])> {
    statements
        .iter()
        .map(|(query, params)| (query.as_str(), params.as_slice()))
        .collect()
}

fn http_runtime() -> PyResult<&'static Runtime> {
    if let Some(runtime) = HTTP_RUNTIME.get() {
        return Ok(runtime);
    }
    let runtime = Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|_| PyRuntimeError::new_err("UQA HTTP runtime could not be initialized"))?;
    let _ = HTTP_RUNTIME.set(runtime);
    HTTP_RUNTIME
        .get()
        .ok_or_else(|| PyRuntimeError::new_err("UQA HTTP runtime could not be initialized"))
}

fn run_http<F, T>(py: Python<'_>, future: F) -> PyResult<T>
where
    F: Future<Output = Result<T, HttpEngineError>> + Send,
    T: Send,
{
    let runtime = http_runtime()?;
    py.detach(move || runtime.block_on(future))
        .map_err(http_runtime_error)
}

fn http_runtime_error(error: HttpEngineError) -> PyErr {
    PyRuntimeError::new_err(error.to_string())
}

fn stream_frame_to_py(py: Python<'_>, frame: SQLStreamFrame) -> PyResult<Py<PyAny>> {
    let output = PyDict::new(py);
    match frame {
        SQLStreamFrame::Metadata {
            columns,
            row_count,
            spilled_to_disk,
            request_id,
        } => {
            output.set_item("type", "metadata")?;
            output.set_item("columns", columns)?;
            output.set_item("row_count", row_count)?;
            output.set_item("spilled_to_disk", spilled_to_disk)?;
            output.set_item("request_id", request_id)?;
        }
        SQLStreamFrame::Row { row } => {
            output.set_item("type", "row")?;
            output.set_item("row", map_to_py(py, &row)?)?;
        }
        SQLStreamFrame::Complete {
            row_count,
            request_id,
        } => {
            output.set_item("type", "complete")?;
            output.set_item("row_count", row_count)?;
            output.set_item("request_id", request_id)?;
        }
        SQLStreamFrame::Error {
            code,
            message,
            request_id,
        } => {
            output.set_item("type", "error")?;
            output.set_item("code", code)?;
            output.set_item("message", message)?;
            output.set_item("request_id", request_id)?;
        }
    }
    Ok(output.into_any().unbind())
}
