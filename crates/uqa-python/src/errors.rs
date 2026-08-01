//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Python and SQL error boundary conversion.

use super::{PyErr, PyRuntimeError, PyStringMethods, PyTypeMethods, Python, SQLError};

pub(super) fn runtime_error(error: impl std::fmt::Display) -> PyErr {
    PyRuntimeError::new_err(error.to_string())
}

pub(super) fn py_error_to_sql(py: Python<'_>, name: &str, action: &str, error: PyErr) -> SQLError {
    let err_type = error.get_type(py).name().map_or_else(
        |_| "PythonError".to_string(),
        |name| name.to_string_lossy().into_owned(),
    );
    SQLError::Internal(format!(
        "Python UDF `{name}` failed during {action}: {err_type}: {}",
        error.value(py)
    ))
}
