//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Destination table, analyzer, FTS, and vector-index installation.

use super::{
    standard_analyzer, table_exists, Connection, Engine, PythonMigrationError,
    PythonMigrationReport, TableSpec,
};

pub(super) fn create_tables(
    _source: &Connection,
    engine: &Engine,
    specs: &[TableSpec],
    report: &mut PythonMigrationReport,
) -> Result<(), PythonMigrationError> {
    for spec in specs {
        engine.create_table(&spec.name, standard_analyzer("english"), Vec::new())?;
        for col in &spec.rust_columns {
            engine.try_register_column(&spec.name, col.clone())?;
        }
        for vector in &spec.vector_fields {
            if vector.dimensions == 0 {
                return Err(PythonMigrationError::Invalid(format!(
                    "VECTOR field {}.{} has unknown dimensions",
                    spec.name, vector.field
                )));
            }
            engine.rebuild_ivf_vector_field(
                &spec.name,
                vector.field.clone(),
                vector.dimensions,
                vector.params,
            )?;
        }
        report.tables += 1;
        report.vector_fields += spec.vector_fields.len();
    }
    Ok(())
}

pub(super) fn migrate_analyzers(
    conn: &Connection,
    engine: &Engine,
) -> Result<usize, PythonMigrationError> {
    if !table_exists(conn, "_analyzers")? {
        return Ok(0);
    }
    let mut stmt = conn.prepare("SELECT name, config_json FROM _analyzers ORDER BY name")?;
    let rows = stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut count = 0;
    for row in rows {
        let (name, config_json) = row?;
        engine
            .register_named_analyzer(&name, &config_json)
            .map_err(PythonMigrationError::Invalid)?;
        count += 1;
    }
    Ok(count)
}

pub(super) fn migrate_table_field_analyzers(
    conn: &Connection,
    engine: &Engine,
) -> Result<usize, PythonMigrationError> {
    if !table_exists(conn, "_table_field_analyzers")? {
        return Ok(0);
    }
    let mut stmt = conn.prepare(
        "SELECT table_name, field, phase, analyzer_name FROM _table_field_analyzers
         ORDER BY table_name, field, phase",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
        ))
    })?;
    let mut count = 0;
    for row in rows {
        let (table, field, phase, analyzer_name) = row?;
        engine
            .set_table_field_analyzer(&table, &field, &analyzer_name, &phase)
            .map_err(PythonMigrationError::Invalid)?;
        count += 1;
    }
    Ok(count)
}

pub(super) fn install_secondary_indexes(
    engine: &Engine,
    specs: &[TableSpec],
    report: &mut PythonMigrationReport,
) -> Result<(), PythonMigrationError> {
    for spec in specs {
        for field in &spec.fts_fields {
            engine
                .add_fts_field(&spec.name, field.clone())
                .map_err(PythonMigrationError::Invalid)?;
            report.fts_fields += 1;
        }
    }
    Ok(())
}
