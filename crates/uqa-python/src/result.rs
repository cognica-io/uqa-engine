//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Python wrapper for SQL result sets.

use pyo3::types::{PyList, PyListMethods, PyString};
use std::collections::HashMap;

use super::{
    pyclass, pymethods, value_to_py, BTreeMap, Bound, Py, PyAny, PyDict, PyDictMethods, PyResult,
    Python, SQLResult, Value,
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
    fn columns<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyList>> {
        PyList::new(
            py,
            self.columns.iter().map(|name| PyString::intern(py, name)),
        )
    }

    #[getter]
    fn affected_rows(&self) -> u64 {
        self.affected_rows
    }

    #[getter]
    fn rows(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let result = PyList::empty(py);
        let mut names = HashMap::with_capacity(self.columns.len());
        for row in &self.rows {
            let dict = PyDict::new(py);
            for (name, value) in row {
                let key = names
                    .entry(name.as_str())
                    .or_insert_with(|| PyString::intern(py, name));
                dict.set_item(&*key, value_to_py(py, value)?)?;
            }
            result.append(dict)?;
        }
        Ok(result.into_any().unbind())
    }

    fn to_dict(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let dict = PyDict::new(py);
        dict.set_item("columns", self.columns(py)?)?;
        dict.set_item("rows", self.rows(py)?)?;
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
