//! Secondary catalog metadata, statistics, models, FDWs, and path indexes.

use super::{
    column_to_rust, json_object_to_pairs, table_columns, table_exists, CatalogIndex,
    ColumnStatsInput, Connection, Engine, PythonColumnDef, PythonMigrationError,
};

pub(super) fn persist_catalog_indexes(
    engine: &Engine,
    indexes: &[CatalogIndex],
) -> Result<usize, PythonMigrationError> {
    for idx in indexes {
        let options = idx
            .parameters
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect::<Vec<_>>();
        engine.register_catalog_index(
            &idx.name,
            &idx.index_type,
            &idx.table_name,
            &idx.columns,
            &options,
        )?;
    }
    Ok(indexes.len())
}

pub(super) fn migrate_column_stats(
    conn: &Connection,
    engine: &Engine,
) -> Result<usize, PythonMigrationError> {
    if !table_exists(conn, "_column_stats")? {
        return Ok(0);
    }
    let Some(catalog) = engine.catalog.as_ref() else {
        return Ok(0);
    };
    let mut stmt = conn.prepare(
        "SELECT table_name, column_name, distinct_count, null_count,
                min_value, max_value, row_count, histogram, mcv_values, mcv_frequencies
           FROM _column_stats
          ORDER BY table_name, column_name",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, i64>(3)?,
            row.get::<_, Option<String>>(4)?,
            row.get::<_, Option<String>>(5)?,
            row.get::<_, i64>(6)?,
            row.get::<_, String>(7)?,
            row.get::<_, String>(8)?,
            row.get::<_, String>(9)?,
        ))
    })?;
    let mut count = 0;
    for row in rows {
        let (
            table_name,
            column_name,
            distinct_count,
            null_count,
            min_value,
            max_value,
            row_count,
            histogram_json,
            mcv_values_json,
            mcv_frequencies_json,
        ) = row?;
        catalog.save_column_stats(ColumnStatsInput {
            table_name: &table_name,
            column_name: &column_name,
            distinct_count,
            null_count,
            min_value: min_value.as_deref(),
            max_value: max_value.as_deref(),
            row_count,
            histogram_json: &histogram_json,
            mcv_values_json: &mcv_values_json,
            mcv_frequencies_json: &mcv_frequencies_json,
        })?;
        count += 1;
    }
    Ok(count)
}

pub(super) fn migrate_scoring_params(
    conn: &Connection,
    engine: &Engine,
) -> Result<usize, PythonMigrationError> {
    if !table_exists(conn, "_scoring_params")? {
        return Ok(0);
    }
    let cols = table_columns(conn, "_scoring_params")?;
    let params_col = if cols.iter().any(|col| col == "params_json") {
        "params_json"
    } else {
        "params"
    };
    let sql = format!("SELECT name, {params_col} FROM _scoring_params ORDER BY name");
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut count = 0;
    for row in rows {
        let (name, params_json) = row?;
        engine.save_scoring_params(&name, &params_json)?;
        count += 1;
    }
    Ok(count)
}

pub(super) fn migrate_models(
    conn: &Connection,
    engine: &Engine,
) -> Result<usize, PythonMigrationError> {
    if !table_exists(conn, "_models")? {
        return Ok(0);
    }
    let cols = table_columns(conn, "_models")?;
    let name_col = if cols.iter().any(|col| col == "model_name") {
        "model_name"
    } else {
        "name"
    };
    let body_col = if cols.iter().any(|col| col == "config_json") {
        "config_json"
    } else {
        "body"
    };
    let Some(catalog) = engine.catalog.as_ref() else {
        return Ok(0);
    };
    let sql = format!("SELECT {name_col}, {body_col} FROM _models ORDER BY {name_col}");
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut count = 0;
    for row in rows {
        let (name, body) = row?;
        catalog.save_model(&name, &body)?;
        count += 1;
    }
    Ok(count)
}

pub(super) fn migrate_foreign_servers(
    conn: &Connection,
    engine: &Engine,
) -> Result<usize, PythonMigrationError> {
    if !table_exists(conn, "_foreign_servers")? {
        return Ok(0);
    }
    let mut stmt =
        conn.prepare("SELECT name, fdw_type, options FROM _foreign_servers ORDER BY name")?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    let mut count = 0;
    for row in rows {
        let (name, fdw_type, options_json) = row?;
        let options = json_object_to_pairs(&options_json)?;
        engine
            .register_foreign_server(name, fdw_type, options, true)
            .map_err(PythonMigrationError::Invalid)?;
        count += 1;
    }
    Ok(count)
}

pub(super) fn migrate_foreign_tables(
    conn: &Connection,
    engine: &Engine,
) -> Result<usize, PythonMigrationError> {
    if !table_exists(conn, "_foreign_tables")? {
        return Ok(0);
    }
    let mut stmt = conn.prepare(
        "SELECT name, server_name, columns_json, options FROM _foreign_tables ORDER BY name",
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
        let (name, server_name, columns_json, options_json) = row?;
        let columns: Vec<PythonColumnDef> = serde_json::from_str(&columns_json)?;
        let rust_columns = columns
            .iter()
            .map(column_to_rust)
            .collect::<Result<Vec<_>, _>>()?;
        let options = json_object_to_pairs(&options_json)?;
        engine
            .register_foreign_table(name, server_name, rust_columns, options, true)
            .map_err(PythonMigrationError::Invalid)?;
        count += 1;
    }
    Ok(count)
}

pub(super) fn migrate_path_indexes(
    conn: &Connection,
    engine: &Engine,
) -> Result<usize, PythonMigrationError> {
    if !table_exists(conn, "_path_indexes")? {
        return Ok(0);
    }
    let mut stmt =
        conn.prepare("SELECT graph_name, label_sequences FROM _path_indexes ORDER BY graph_name")?;
    let rows = stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut count = 0;
    for row in rows {
        let (graph_name, labels_json) = row?;
        let label_sequences: Vec<Vec<String>> = serde_json::from_str(&labels_json)?;
        if engine.has_graph(&graph_name)? {
            engine.build_path_index("default", &graph_name, &label_sequences)?;
            count += 1;
        }
    }
    Ok(count)
}
