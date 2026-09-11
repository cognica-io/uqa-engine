//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Convert projected training rows into ML examples while preserving row diagnostics.

use super::TrainingTables;
use uqa_core::Value;
use uqa_ml::{TrainingExample, TrainingSet};
use uqa_sql::SQLError;

fn value_to_f64_vec(value: &Value) -> Result<Vec<f64>, String> {
    match value {
        Value::List(items) => items
            .iter()
            .map(|item| match item {
                Value::Float(value) => Ok(*value),
                Value::Int(value) => Ok(*value as f64),
                Value::Decimal(value) => value
                    .to_f64()
                    .ok_or_else(|| "decimal feature is outside f64 range".to_string()),
                other => Err(format!("expected numeric feature, got {other:?}")),
            })
            .collect(),
        Value::Array(array) if array.dimensions().len() <= 1 => array
            .elements()
            .iter()
            .map(|item| match item {
                Value::Float(value) => Ok(*value),
                Value::Int(value) => Ok(*value as f64),
                Value::Decimal(value) => value
                    .to_f64()
                    .ok_or_else(|| "decimal feature is outside f64 range".to_string()),
                other => Err(format!("expected numeric feature, got {other:?}")),
            })
            .collect(),
        Value::Array(array) => Err(format!(
            "expected one-dimensional feature array, got {} dimensions",
            array.dimensions().len()
        )),
        other => Err(format!("expected feature array, got {other:?}")),
    }
}

fn value_to_usize(value: &Value) -> Result<usize, String> {
    match value {
        Value::Int(value) if *value >= 0 => usize::try_from(*value)
            .map_err(|_| format!("integer label {value} exceeds the platform usize range")),
        Value::Float(value) => {
            let exponent = i32::try_from(usize::BITS)
                .map_err(|_| "platform usize width exceeds f64 exponent range".to_string())?;
            let upper_exclusive = 2.0_f64.powi(exponent);
            if !value.is_finite()
                || *value < 0.0
                || value.fract() != 0.0
                || *value >= upper_exclusive
            {
                return Err(format!(
                    "expected finite non-negative integer label within usize range, got {value}"
                ));
            }
            Ok(*value as usize)
        }
        other => Err(format!(
            "expected non-negative integer label, got {other:?}"
        )),
    }
}

pub(super) fn training_set_from_table(
    tables: &dyn TrainingTables,
    table: &str,
    features_field: &str,
    label_field: &str,
) -> Result<TrainingSet, SQLError> {
    let table_state = tables
        .training_table(table)
        .map_err(|err| SQLError::Internal(format!("resolve table `{table}`: {err}")))?
        .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
    let doc_ids = table_state
        .doc_ids()
        .map_err(|err| SQLError::Internal(format!("scan deep_learn table `{table}`: {err}")))?;
    let projection = vec![features_field.to_string(), label_field.to_string()];
    let documents = tables.training_documents(table, &doc_ids, &projection)?;
    let mut examples = Vec::new();
    for (doc_id, document) in documents {
        let features = document.get(features_field).ok_or_else(|| {
            SQLError::TypeMismatch(format!(
                "deep_learn table {table:?} row {doc_id} is missing `{features_field}`"
            ))
        })?;
        let label = document.get(label_field).ok_or_else(|| {
            SQLError::TypeMismatch(format!(
                "deep_learn table {table:?} row {doc_id} is missing `{label_field}`"
            ))
        })?;
        examples.push(TrainingExample {
            features: value_to_f64_vec(features).map_err(|e| {
                SQLError::TypeMismatch(format!(
                    "deep_learn table {table:?} row {doc_id} `{features_field}`: {e}"
                ))
            })?,
            label: value_to_usize(label).map_err(|e| {
                SQLError::TypeMismatch(format!(
                    "deep_learn table {table:?} row {doc_id} `{label_field}`: {e}"
                ))
            })?,
        });
    }
    Ok(TrainingSet {
        examples,
        class_count: None,
    })
}

#[cfg(test)]
mod tests;
