//! Cypher execution and graph catalog operations.

use super::{
    document_from_py, pymethods, runtime_error, Bound, PyAny, PyEngine, PyResult, PySQLResult,
    Python,
};

#[pymethods]
impl PyEngine {
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

    fn create_graph(&self, name: &str) -> PyResult<bool> {
        self.inner.create_graph(name).map_err(runtime_error)
    }

    fn drop_graph(&self, name: &str) -> PyResult<bool> {
        self.inner.drop_graph(name).map_err(runtime_error)
    }

    fn list_graphs(&self) -> PyResult<Vec<String>> {
        self.inner.list_graphs().map_err(runtime_error)
    }
}
