//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Vector extraction, fallback loading, and finite f32 validation.

use super::{
    quote_ident, table_exists, BTreeMap, Connection, DocId, PythonMigrationError, TableSpec, Value,
    VectorSpec,
};

pub(super) fn extract_vectors(
    document: &BTreeMap<String, Value>,
    specs: &[VectorSpec],
) -> Result<BTreeMap<String, Vec<f32>>, PythonMigrationError> {
    let mut out = BTreeMap::new();
    for spec in specs {
        if let Some(value) = document.get(&spec.field) {
            if let Some(vector) = value_to_f32_vec(value)? {
                out.insert(spec.field.clone(), vector);
            }
        }
    }
    Ok(out)
}

pub(super) fn value_to_f32_vec(value: &Value) -> Result<Option<Vec<f32>>, PythonMigrationError> {
    match value {
        Value::List(items) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                match item {
                    Value::Int(n) => out.push(migration_f32(*n as f64)?),
                    Value::Float(n) => out.push(migration_f32(*n)?),
                    other => {
                        return Err(PythonMigrationError::Invalid(format!(
                            "vector list contains non-numeric value {other:?}"
                        )))
                    }
                }
            }
            Ok(Some(out))
        }
        Value::Bytes(bytes) => blob_to_f32_vec(bytes).map(Some),
        Value::Null => Ok(None),
        other => Err(PythonMigrationError::Invalid(format!(
            "vector field contains unsupported value {other:?}"
        ))),
    }
}

pub(super) fn migration_f32(value: f64) -> Result<f32, PythonMigrationError> {
    if !value.is_finite() || value < -f64::from(f32::MAX) || value > f64::from(f32::MAX) {
        return Err(PythonMigrationError::Invalid(format!(
            "vector value {value:?} is outside the finite f32 range"
        )));
    }
    Ok(value as f32)
}

pub(super) fn vector_value(vector: &[f32]) -> Value {
    Value::List(
        vector
            .iter()
            .map(|value| Value::Float(f64::from(*value)))
            .collect(),
    )
}

pub(super) fn read_vector_fallbacks(
    conn: &Connection,
    spec: &TableSpec,
) -> Result<BTreeMap<(DocId, String), Vec<f32>>, PythonMigrationError> {
    let mut out = BTreeMap::new();
    for vector in &spec.vector_fields {
        let table_name = format!("_ivf_lists_{}_{}", spec.name, vector.field);
        if table_exists(conn, &table_name)? {
            let sql = format!(
                "SELECT doc_id, embedding FROM {} ORDER BY doc_id",
                quote_ident(&table_name)
            );
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt.query_map([], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?))
            })?;
            for row in rows {
                let (doc_id, blob) = row?;
                out.insert(
                    (doc_id as DocId, vector.field.clone()),
                    blob_to_f32_vec(&blob)?,
                );
            }
        }
    }
    Ok(out)
}

pub(super) fn blob_to_f32_vec(blob: &[u8]) -> Result<Vec<f32>, PythonMigrationError> {
    if !blob.len().is_multiple_of(4) {
        return Err(PythonMigrationError::Invalid(format!(
            "vector blob length {} is not divisible by 4",
            blob.len()
        )));
    }
    blob.chunks_exact(4)
        .map(|chunk| {
            let value = f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
            if value.is_finite() {
                Ok(value)
            } else {
                Err(PythonMigrationError::Invalid(format!(
                    "vector blob contains non-finite value {value:?}"
                )))
            }
        })
        .collect()
}
