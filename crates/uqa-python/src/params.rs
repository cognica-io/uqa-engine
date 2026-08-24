//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Python SQL parameter wrapper and vector/tensor constructors.

use super::{
    pyclass, pyfunction, pymethods, validate_tensor, validate_vector, value_from_py, Bound, PyAny,
    PyResult, SQLParam,
};

#[pyclass(name = "SQLParam", module = "uqa._uqa", skip_from_py_object)]
#[derive(Clone)]
pub(super) struct PySQLParam {
    pub(super) inner: SQLParam,
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
    fn vector(values: Vec<f32>) -> PyResult<Self> {
        Ok(Self {
            inner: SQLParam::vector(validate_vector(values, "SQL vector parameter")?),
        })
    }

    #[staticmethod]
    fn tensor(values: Vec<Vec<f32>>) -> PyResult<Self> {
        Ok(Self {
            inner: SQLParam::tensor(validate_tensor(values, "SQL tensor parameter")?),
        })
    }

    fn __repr__(&self) -> String {
        match &self.inner {
            SQLParam::Scalar(value) | SQLParam::TypedScalar { value, .. } => {
                format!("SQLParam.scalar({value:?})")
            }
            SQLParam::Vector(values) => format!("SQLParam.vector(len={})", values.len()),
            SQLParam::Tensor(values) => format!("SQLParam.tensor(rows={})", values.len()),
        }
    }
}

#[pyfunction]
pub(super) fn vector(values: Vec<f32>) -> PyResult<PySQLParam> {
    PySQLParam::vector(values)
}

#[pyfunction]
pub(super) fn tensor(values: Vec<Vec<f32>>) -> PyResult<PySQLParam> {
    PySQLParam::tensor(values)
}

#[pyfunction]
pub(super) fn scalar(value: &Bound<'_, PyAny>) -> PyResult<PySQLParam> {
    PySQLParam::scalar(value)
}
