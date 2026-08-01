//! Document, vector, tensor, and binary-label validation.

use super::{
    value_from_py, BTreeMap, Bound, PyAny, PyAnyMethods, PyDict, PyDictMethods, PyResult,
    PyTypeError, PyValueError, Value,
};

pub(super) fn document_from_py(value: &Bound<'_, PyAny>) -> PyResult<BTreeMap<String, Value>> {
    let dict = value
        .cast::<PyDict>()
        .map_err(|_| PyTypeError::new_err("expected a dict"))?;
    let mut out = BTreeMap::new();
    for (key, value) in dict.iter() {
        out.insert(key.extract::<String>()?, value_from_py(&value)?);
    }
    Ok(out)
}

pub(super) fn vector_values_from_py(
    value: &Bound<'_, PyAny>,
) -> PyResult<BTreeMap<String, Vec<Vec<f32>>>> {
    let dict = value
        .cast::<PyDict>()
        .map_err(|_| PyTypeError::new_err("expected a dict of vector fields"))?;
    let mut out = BTreeMap::new();
    for (key, value) in dict.iter() {
        let field = key.extract::<String>()?;
        let vectors = if let Ok(single) = value.extract::<Vec<f32>>() {
            vec![single]
        } else {
            value.extract::<Vec<Vec<f32>>>()?
        };
        let vectors = validate_tensor(vectors, &format!("vector field `{field}`"))?;
        out.insert(field, vectors);
    }
    Ok(out)
}

pub(super) fn validate_vector(values: Vec<f32>, context: &str) -> PyResult<Vec<f32>> {
    for (index, value) in values.iter().enumerate() {
        if !value.is_finite() {
            return Err(PyValueError::new_err(format!(
                "{context}[{index}] must be finite, got {value}"
            )));
        }
    }
    Ok(values)
}

pub(super) fn validate_tensor(values: Vec<Vec<f32>>, context: &str) -> PyResult<Vec<Vec<f32>>> {
    values
        .into_iter()
        .enumerate()
        .map(|(row, values)| validate_vector(values, &format!("{context}[{row}]")))
        .collect()
}

pub(super) fn validate_binary_label(label: u8) -> PyResult<u8> {
    if label <= 1 {
        Ok(label)
    } else {
        Err(PyValueError::new_err(
            "labels must contain only binary values 0 or 1",
        ))
    }
}

pub(super) fn validate_binary_labels(labels: Vec<u8>) -> PyResult<Vec<u8>> {
    labels.into_iter().map(validate_binary_label).collect()
}
