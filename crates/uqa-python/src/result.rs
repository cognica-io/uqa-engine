//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Python wrapper for SQL result sets.

use super::{
    pyclass, pymethods, rows_to_py, BTreeMap, Py, PyAny, PyDict, PyDictMethods, PyResult, Python,
    SQLResult, Value,
};

#[pyclass(name = "SQLResult", module = "uqa._uqa", skip_from_py_object)]
#[derive(Clone)]
pub(super) struct PySQLResult {
    pub(super) columns: Vec<String>,
    pub(super) rows: Vec<BTreeMap<String, Value>>,
    pub(super) affected_rows: u64,
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
