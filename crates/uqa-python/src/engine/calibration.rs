//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Scoring calibration, learning, and parameter persistence.

use super::{
    calibration_report_to_py, float_map_from_py, float_map_to_py, parse_scoring_params, pymethods,
    runtime_error, validate_binary_label, validate_binary_labels, Bound, Py, PyAny, PyDict,
    PyDictMethods, PyEngine, PyResult, PyValueError, Python,
};

#[pymethods]
impl PyEngine {
    #[pyo3(signature = (table, field, n_samples=50, tokens_per_query=5, seed=42))]
    fn estimate_scoring_params(
        &self,
        py: Python<'_>,
        table: &str,
        field: &str,
        n_samples: usize,
        tokens_per_query: usize,
        seed: i64,
    ) -> PyResult<Py<PyAny>> {
        let inner = self.inner()?;
        let params = py
            .detach(|| {
                inner.estimate_scoring_params(table, field, n_samples, tokens_per_query, seed)
            })
            .map_err(runtime_error)?;
        float_map_to_py(py, &params)
    }

    fn learn_scoring_params(
        &self,
        py: Python<'_>,
        table: &str,
        field: &str,
        query: &str,
        labels: Vec<u8>,
    ) -> PyResult<Py<PyAny>> {
        let labels = validate_binary_labels(labels)?;
        let inner = self.inner()?;
        let params = py
            .detach(|| inner.learn_scoring_params(table, field, query, &labels))
            .map_err(runtime_error)?;
        float_map_to_py(py, &params)
    }

    fn update_scoring_params(
        &self,
        table: &str,
        field: &str,
        score: f64,
        label: u8,
    ) -> PyResult<()> {
        if !score.is_finite() {
            return Err(PyValueError::new_err("score must be finite"));
        }
        let label = validate_binary_label(label)?;
        self.inner()?
            .update_scoring_params(table, field, score, label)
            .map_err(runtime_error)
    }

    fn calibration_report(
        &self,
        py: Python<'_>,
        table: &str,
        field: &str,
        query: &str,
        labels: Vec<u8>,
    ) -> PyResult<Py<PyAny>> {
        let labels = validate_binary_labels(labels)?;
        let inner = self.inner()?;
        let report = py
            .detach(|| inner.calibration_report(table, field, query, &labels))
            .map_err(runtime_error)?;
        calibration_report_to_py(py, &report)
    }

    fn save_scoring_params(&self, name: &str, params: &Bound<'_, PyAny>) -> PyResult<()> {
        let params = float_map_from_py(params)?;
        let json = serde_json::to_string(&params)
            .map_err(|err| PyValueError::new_err(format!("serialize scoring params: {err}")))?;
        self.inner()?
            .save_scoring_params(name, &json)
            .map_err(runtime_error)
    }

    fn load_scoring_params(&self, py: Python<'_>, name: &str) -> PyResult<Py<PyAny>> {
        match self
            .inner()?
            .load_scoring_params(name)
            .map_err(runtime_error)?
        {
            Some(json) => float_map_to_py(py, &parse_scoring_params(name, &json)?),
            None => Ok(py.None()),
        }
    }

    fn load_all_scoring_params(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let dict = PyDict::new(py);
        for (name, json) in self
            .inner()?
            .load_all_scoring_params()
            .map_err(runtime_error)?
        {
            let params = parse_scoring_params(&name, &json)?;
            dict.set_item(name, float_map_to_py(py, &params)?)?;
        }
        Ok(dict.into_any().unbind())
    }

    fn drop_scoring_params(&self, name: &str) -> PyResult<bool> {
        self.inner()?
            .drop_scoring_params(name)
            .map_err(runtime_error)
    }
}
