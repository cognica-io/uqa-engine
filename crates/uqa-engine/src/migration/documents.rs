//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Document extraction, coercion, and insertion.

use super::{
    convert_value_to_column_type, extract_vectors, json_to_value, quote_ident,
    read_vector_fallbacks, sqlite_value_to_uqa, table_columns, table_exists, vector_value,
    BTreeMap, ColumnDef, Connection, DocId, Engine, MigratedDocument, PythonMigrationError,
    PythonMigrationReport, TableSpec, Value, ValueRef,
};

pub(super) fn migrate_documents(
    conn: &Connection,
    engine: &Engine,
    specs: &[TableSpec],
    report: &mut PythonMigrationReport,
) -> Result<(), PythonMigrationError> {
    for spec in specs {
        let mut rows = read_table_documents(conn, spec)?;
        if rows.is_empty() {
            rows = read_shared_documents(conn, spec)?;
        }
        let vector_fallbacks = read_vector_fallbacks(conn, spec)?;
        for (doc_id, document) in rows {
            let mut document =
                coerce_migrated_document(document, &spec.rust_columns).map_err(|error| {
                    PythonMigrationError::Invalid(format!(
                        "coerce migrated row {doc_id} in table {}: {error}",
                        spec.name
                    ))
                })?;
            let mut vectors = extract_vectors(&document, &spec.vector_fields)?;
            for vector in &spec.vector_fields {
                if vectors.contains_key(&vector.field) {
                    continue;
                }
                if let Some(value) = vector_fallbacks.get(&(doc_id, vector.field.clone())) {
                    vectors.insert(vector.field.clone(), value.clone());
                    document
                        .entry(vector.field.clone())
                        .or_insert_with(|| vector_value(value));
                }
            }
            engine.add_document_with_vectors(&spec.name, doc_id, document, vectors)?;
            report.documents += 1;
        }
    }
    Ok(())
}

pub(super) fn coerce_migrated_document(
    mut document: BTreeMap<String, Value>,
    columns: &[ColumnDef],
) -> Result<BTreeMap<String, Value>, uqa_sql::SQLError> {
    for column in columns {
        let Some(value) = document.remove(&column.name) else {
            continue;
        };
        document.insert(
            column.name.clone(),
            convert_value_to_column_type(value, &column.ty)?,
        );
    }
    Ok(document)
}

pub(super) fn read_table_documents(
    conn: &Connection,
    spec: &TableSpec,
) -> Result<Vec<MigratedDocument>, PythonMigrationError> {
    let table_name = format!("_data_{}", spec.name);
    if !table_exists(conn, &table_name)? {
        return Ok(Vec::new());
    }
    let select_cols = spec
        .columns
        .iter()
        .map(|col| quote_ident(&col.name))
        .collect::<Vec<_>>();
    let sql = if select_cols.is_empty() {
        format!("SELECT _rowid FROM {}", quote_ident(&table_name))
    } else {
        format!(
            "SELECT _rowid, {} FROM {} ORDER BY _rowid",
            select_cols.join(", "),
            quote_ident(&table_name)
        )
    };
    let mut stmt = conn.prepare(&sql)?;
    let mut cursor = stmt.query([])?;
    let mut out = Vec::new();
    while let Some(row) = cursor.next()? {
        let raw_id = row.get::<_, i64>(0)?;
        if raw_id < 0 {
            return Err(PythonMigrationError::Invalid(format!(
                "negative doc id {raw_id} in table {}",
                spec.name
            )));
        }
        let mut document = BTreeMap::new();
        for (idx, col) in spec.columns.iter().enumerate() {
            let raw = row.get_ref(idx + 1)?;
            if matches!(raw, ValueRef::Null) {
                continue;
            }
            let value = sqlite_value_to_uqa(raw, col)?;
            if !matches!(value, Value::Null) {
                document.insert(col.name.clone(), value);
            }
        }
        out.push((raw_id as DocId, document));
    }
    Ok(out)
}

pub(super) fn read_shared_documents(
    conn: &Connection,
    spec: &TableSpec,
) -> Result<Vec<MigratedDocument>, PythonMigrationError> {
    if !table_exists(conn, "_documents")? {
        return Ok(Vec::new());
    }
    let cols = table_columns(conn, "_documents")?;
    let body_col = if cols.iter().any(|col| col == "data_json") {
        "data_json"
    } else if cols.iter().any(|col| col == "body") {
        "body"
    } else {
        return Ok(Vec::new());
    };
    let sql =
        format!("SELECT doc_id, {body_col} FROM _documents WHERE table_name = ?1 ORDER BY doc_id");
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([spec.name.as_str()], |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (raw_id, body) = row?;
        let json: serde_json::Value = serde_json::from_str(&body)?;
        let Value::Map(map) = json_to_value(&json)? else {
            return Err(PythonMigrationError::Invalid(format!(
                "document {raw_id} in table {} is not a JSON object",
                spec.name
            )));
        };
        out.push((raw_id as DocId, map));
    }
    Ok(out)
}
