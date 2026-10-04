//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Notices, limits, cancellation, close, and representation.

use super::{
    pymethods, runtime_error, Py, PyAny, PyDict, PyDictMethods, PyEngine, PyResult, PyRuntimeError,
    Python,
};

#[pymethods]
impl PyEngine {
    fn take_sql_notices(&self, py: Python<'_>) -> PyResult<Vec<Py<PyAny>>> {
        self.inner()?
            .take_sql_notices()
            .into_iter()
            .map(|notice| {
                let dict = PyDict::new(py);
                dict.set_item("level", notice.level.as_str())?;
                dict.set_item("sqlstate", notice.sqlstate)?;
                dict.set_item("message", notice.message)?;
                dict.set_item("detail", notice.detail)?;
                dict.set_item("hint", notice.hint)?;
                Ok(dict.into_any().unbind())
            })
            .collect()
    }

    fn sql_function_depth_limit(&self) -> PyResult<usize> {
        Ok(self.inner()?.sql_function_depth_limit())
    }

    fn set_sql_function_depth_limit(&self, limit: usize) -> PyResult<()> {
        self.inner()?.set_sql_function_depth_limit(limit);
        Ok(())
    }

    fn cancel(&self) -> PyResult<()> {
        self.inner()?.cancel();
        Ok(())
    }

    fn close(&mut self) -> PyResult<()> {
        let Some(inner) = self.inner.take() else {
            return Ok(());
        };
        if let Err(error) = inner.close() {
            self.inner = Some(inner);
            return Err(PyRuntimeError::new_err(format!("close engine: {error}")));
        }
        Ok(())
    }

    fn __repr__(&self) -> PyResult<String> {
        let Some(inner) = self.inner.as_ref() else {
            return Ok("Engine(closed=True)".to_string());
        };
        Ok(format!(
            "Engine(tables={:?})",
            inner.table_names().map_err(runtime_error)?
        ))
    }
}
