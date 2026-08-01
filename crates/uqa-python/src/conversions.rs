//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL parameters, batches, and table-function row conversion.

use super::{
    value_from_py, value_to_py, Bound, Py, PyAny, PyAnyMethods, PyDict, PyDictMethods, PyIterator,
    PyRef, PyResult, PySQLParam, PyTuple, PyTupleMethods, PyTypeError, PyValueError, Python,
    SQLParam, SQLTableFunctionResult, Value,
};

pub(super) fn params_from_py(params: Option<&Bound<'_, PyAny>>) -> PyResult<Vec<SQLParam>> {
    let Some(params) = params else {
        return Ok(Vec::new());
    };
    if params.is_none() {
        return Ok(Vec::new());
    }
    let iterator = PyIterator::from_object(params)?;
    iterator
        .map(|item| param_from_py(&item?))
        .collect::<PyResult<Vec<_>>>()
}

pub(super) fn param_from_py(value: &Bound<'_, PyAny>) -> PyResult<SQLParam> {
    if let Ok(param) = value.extract::<PyRef<'_, PySQLParam>>() {
        return Ok(param.inner.clone());
    }
    Ok(SQLParam::scalar(value_from_py(value)?))
}

pub(super) fn batch_from_py(
    statements: &Bound<'_, PyAny>,
) -> PyResult<Vec<(String, Vec<SQLParam>)>> {
    let iterator = PyIterator::from_object(statements)?;
    iterator
        .map(|item| {
            let item = item?;
            let tuple = item.cast::<pyo3::types::PyTuple>().map_err(|_| {
                PyTypeError::new_err("sql_batch entries must be (sql, params) tuples")
            })?;
            if tuple.len() != 2 {
                return Err(PyValueError::new_err(
                    "sql_batch entries must contain exactly two values",
                ));
            }
            let sql = tuple.get_item(0)?.extract::<String>()?;
            let params = params_from_py(Some(&tuple.get_item(1)?))?;
            Ok((sql, params))
        })
        .collect()
}

pub(super) fn ensure_callable(py: Python<'_>, callable: &Py<PyAny>, label: &str) -> PyResult<()> {
    if callable.bind(py).is_callable() {
        Ok(())
    } else {
        Err(PyTypeError::new_err(format!("{label} must be callable")))
    }
}

pub(super) fn values_to_py_tuple<'py>(
    py: Python<'py>,
    values: &[Value],
) -> PyResult<Bound<'py, PyTuple>> {
    let items = values
        .iter()
        .map(|value| value_to_py(py, value))
        .collect::<PyResult<Vec<_>>>()?;
    PyTuple::new(py, items)
}

pub(super) fn table_function_result_from_py(
    value: &Bound<'_, PyAny>,
) -> PyResult<SQLTableFunctionResult> {
    if let Ok(dict) = value.cast::<PyDict>() {
        let columns_obj = dict
            .get_item("columns")?
            .ok_or_else(|| PyValueError::new_err("table function dict result needs `columns`"))?;
        let rows_obj = dict
            .get_item("rows")?
            .ok_or_else(|| PyValueError::new_err("table function dict result needs `rows`"))?;
        let mut columns = columns_obj.extract::<Vec<String>>()?;
        let rows = table_rows_from_py(&rows_obj, &mut columns)?;
        return Ok(SQLTableFunctionResult { columns, rows });
    }

    if let Ok(tuple) = value.cast::<PyTuple>() {
        if tuple.len() == 2 {
            let mut columns = tuple.get_item(0)?.extract::<Vec<String>>()?;
            let rows_obj = tuple.get_item(1)?;
            let rows = table_rows_from_py(&rows_obj, &mut columns)?;
            return Ok(SQLTableFunctionResult { columns, rows });
        }
    }

    let mut columns = Vec::new();
    let rows = table_rows_from_py(value, &mut columns)?;
    Ok(SQLTableFunctionResult { columns, rows })
}

pub(super) fn table_rows_from_py(
    rows_obj: &Bound<'_, PyAny>,
    columns: &mut Vec<String>,
) -> PyResult<Vec<Vec<Value>>> {
    let iterator = PyIterator::from_object(rows_obj)?;
    let mut rows = Vec::new();
    for row in iterator {
        let row = row?;
        if let Ok(dict) = row.cast::<PyDict>() {
            if columns.is_empty() {
                for (key, _) in dict.iter() {
                    columns.push(key.extract::<String>()?);
                }
            }
            let mut values = Vec::with_capacity(columns.len());
            for column in columns.iter() {
                match dict.get_item(column)? {
                    Some(value) => values.push(value_from_py(&value)?),
                    None => values.push(Value::Null),
                }
            }
            rows.push(values);
        } else {
            if columns.is_empty() {
                return Err(PyValueError::new_err(
                    "table function row sequences require explicit columns",
                ));
            }
            let values = PyIterator::from_object(&row)?
                .map(|item| value_from_py(&item?))
                .collect::<PyResult<Vec<_>>>()?;
            if values.len() != columns.len() {
                return Err(PyValueError::new_err(format!(
                    "table function row has {} values but {} columns",
                    values.len(),
                    columns.len()
                )));
            }
            rows.push(values);
        }
    }
    Ok(rows)
}
