//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bidirectional conversion between Python objects and UQA values.

use super::{
    BTreeMap, Bound, DecimalValue, IntoPyObjectExt, Py, PyAny, PyAnyMethods, PyBool, PyBytes,
    PyBytesMethods, PyDict, PyDictMethods, PyFloat, PyInt, PyIterator, PyList, PyListMethods,
    PyResult, PyString, PyTuple, PyTypeError, PyValueError, Python, TemporalValue, Value,
};

pub(super) fn value_from_py(value: &Bound<'_, PyAny>) -> PyResult<Value> {
    if value.is_none() {
        return Ok(Value::Null);
    }
    if value.is_instance_of::<PyBool>() {
        return Ok(Value::Bool(value.extract()?));
    }
    if value.is_instance_of::<PyInt>() {
        return Ok(Value::Int(value.extract()?));
    }
    if value.is_instance_of::<PyFloat>() {
        return Ok(Value::Float(value.extract()?));
    }
    let decimal_type = value.py().import("decimal")?.getattr("Decimal")?;
    if value.is_instance(&decimal_type)? {
        let text = value.str()?.extract::<String>()?;
        return DecimalValue::parse(&text)
            .map(Value::Decimal)
            .ok_or_else(|| PyValueError::new_err(format!("invalid decimal value {text}")));
    }
    if value.is_instance_of::<PyString>() {
        return Ok(Value::Str(value.extract()?));
    }
    if let Ok(bytes) = value.cast::<PyBytes>() {
        return Ok(Value::Bytes(bytes.as_bytes().to_vec()));
    }
    if let Ok(dict) = value.cast::<PyDict>() {
        let mut out = BTreeMap::new();
        for (key, value) in dict.iter() {
            out.insert(key.extract::<String>()?, value_from_py(&value)?);
        }
        return Ok(Value::Map(out));
    }
    let iterator = PyIterator::from_object(value)?;
    let values = iterator
        .map(|item| value_from_py(&item?))
        .collect::<PyResult<Vec<_>>>()?;
    Ok(Value::List(values))
}

pub(super) fn value_to_py(py: Python<'_>, value: &Value) -> PyResult<Py<PyAny>> {
    match value {
        Value::Null => Ok(py.None()),
        Value::Bool(value) => value.into_py_any(py),
        Value::Int(value) => value.into_py_any(py),
        Value::Float(value) => value.into_py_any(py),
        Value::Decimal(value) => decimal_to_py(py, value),
        Value::Str(value) | Value::FixedChar(value) => value.into_py_any(py),
        Value::Json(value) | Value::JsonB(value) => {
            Ok(py.import("json")?.call_method1("loads", (value,))?.unbind())
        }
        Value::Bytes(value) => Ok(PyBytes::new(py, value).into_any().unbind()),
        Value::Temporal(value) => temporal_to_string(value).into_py_any(py),
        Value::Array(array) => {
            let list = PyList::empty(py);
            for value in array.elements() {
                list.append(value_to_py(py, value)?)?;
            }
            Ok(list.into_any().unbind())
        }
        Value::List(values) => {
            let list = PyList::empty(py);
            for value in values {
                list.append(value_to_py(py, value)?)?;
            }
            Ok(list.into_any().unbind())
        }
        Value::Row(values) => {
            let items = values
                .iter()
                .map(|value| value_to_py(py, value))
                .collect::<PyResult<Vec<_>>>()?;
            Ok(PyTuple::new(py, items)?.into_any().unbind())
        }
        Value::Record(values) => record_to_py(py, values),
        Value::Map(values) => map_to_py(py, values),
    }
}

fn record_to_py(py: Python<'_>, values: &[(String, Value)]) -> PyResult<Py<PyAny>> {
    let dict = PyDict::new(py);
    for (key, value) in values {
        dict.set_item(key, value_to_py(py, value)?)?;
    }
    Ok(dict.into_any().unbind())
}

pub(super) fn temporal_to_string(value: &TemporalValue) -> String {
    value.to_sql_string()
}

pub(super) fn decimal_to_py(py: Python<'_>, value: &DecimalValue) -> PyResult<Py<PyAny>> {
    let decimal_type = py.import("decimal")?.getattr("Decimal")?;
    Ok(decimal_type.call1((value.to_sql_string(),))?.unbind())
}

pub(super) fn map_to_py(py: Python<'_>, values: &BTreeMap<String, Value>) -> PyResult<Py<PyAny>> {
    let dict = PyDict::new(py);
    for (key, value) in values {
        dict.set_item(key, value_to_py(py, value)?)?;
    }
    Ok(dict.into_any().unbind())
}

pub(super) fn rows_to_py(py: Python<'_>, rows: &[BTreeMap<String, Value>]) -> PyResult<Py<PyAny>> {
    let list = PyList::empty(py);
    for row in rows {
        list.append(map_to_py(py, row)?)?;
    }
    Ok(list.into_any().unbind())
}

pub(super) fn float_map_to_py(
    py: Python<'_>,
    values: &BTreeMap<String, f64>,
) -> PyResult<Py<PyAny>> {
    let dict = PyDict::new(py);
    for (key, value) in values {
        dict.set_item(key, *value)?;
    }
    Ok(dict.into_any().unbind())
}

pub(super) fn float_map_from_py(value: &Bound<'_, PyAny>) -> PyResult<BTreeMap<String, f64>> {
    let dict = value
        .cast::<PyDict>()
        .map_err(|_| PyTypeError::new_err("expected a dict of float scoring parameters"))?;
    let mut out = BTreeMap::new();
    for (key, value) in dict.iter() {
        let key = key.extract::<String>()?;
        let value = value.extract::<f64>()?;
        if !value.is_finite() {
            return Err(PyValueError::new_err(format!(
                "scoring parameter `{key}` must be finite, got {value}"
            )));
        }
        out.insert(key, value);
    }
    Ok(out)
}
