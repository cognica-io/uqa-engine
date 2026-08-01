//! Notices, limits, cancellation, close, and representation.

use super::{pymethods, runtime_error, PyEngine, PyResult, PyRuntimeError};

#[pymethods]
impl PyEngine {
    fn take_sql_notices(&self) -> Vec<(String, String)> {
        self.inner.take_sql_notices()
    }

    fn sql_function_depth_limit(&self) -> usize {
        self.inner.sql_function_depth_limit()
    }

    fn set_sql_function_depth_limit(&self, limit: usize) {
        self.inner.set_sql_function_depth_limit(limit);
    }

    fn cancel(&self) {
        self.inner.cancel();
    }

    fn close(&self) -> PyResult<()> {
        self.inner
            .close()
            .map_err(|err| PyRuntimeError::new_err(format!("close engine: {err}")))
    }

    fn __repr__(&self) -> PyResult<String> {
        Ok(format!(
            "Engine(tables={:?})",
            self.inner.table_names().map_err(runtime_error)?
        ))
    }
}
