//! Relational, analyzer, and foreign catalog inspection.

use super::{pymethods, runtime_error, PyEngine, PyResult, PyRuntimeError};

#[pymethods]
impl PyEngine {
    fn list_path_indexes(&self) -> PyResult<Vec<String>> {
        self.inner.list_path_indexes().map_err(runtime_error)
    }

    fn table_names(&self) -> PyResult<Vec<String>> {
        self.inner.table_names().map_err(runtime_error)
    }

    fn list_views(&self) -> PyResult<Vec<String>> {
        self.inner.list_views().map_err(runtime_error)
    }

    fn list_schemas(&self) -> PyResult<Vec<String>> {
        self.inner.list_schemas().map_err(runtime_error)
    }

    fn list_sequences(&self) -> PyResult<Vec<String>> {
        self.inner
            .list_sequences()
            .map_err(|err| PyRuntimeError::new_err(format!("list sequences: {err}")))
    }

    fn list_named_analyzers(&self) -> PyResult<Vec<String>> {
        self.inner.list_named_analyzers().map_err(runtime_error)
    }

    fn list_foreign_servers(&self) -> PyResult<Vec<String>> {
        self.inner.list_foreign_servers().map_err(runtime_error)
    }

    fn list_foreign_tables(&self) -> PyResult<Vec<String>> {
        self.inner.list_foreign_tables().map_err(runtime_error)
    }
}
