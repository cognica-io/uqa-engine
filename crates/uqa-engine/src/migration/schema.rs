//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Python catalog schema loading and Rust column/index inference.

use super::{
    json_to_value, parameters_to_string_map, table_exists, BTreeMap, BTreeSet, CatalogIndex,
    ColumnDef, ColumnType, Connection, Expr, IVFIndexParams, PythonColumnDef, PythonMigrationError,
    TableSpec, VectorSpec,
};

pub(super) fn load_catalog_indexes(
    conn: &Connection,
) -> Result<Vec<CatalogIndex>, PythonMigrationError> {
    if !table_exists(conn, "_catalog_indexes")? {
        return Ok(Vec::new());
    }
    let mut stmt = conn.prepare(
        "SELECT name, index_type, table_name, columns, parameters FROM _catalog_indexes",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (name, index_type, table_name, columns_json, parameters_json) = row?;
        out.push(CatalogIndex {
            name,
            index_type,
            table_name,
            columns: serde_json::from_str(&columns_json)?,
            parameters: parameters_to_string_map(&parameters_json)?,
        });
    }
    Ok(out)
}

pub(super) fn load_table_specs(
    conn: &Connection,
    indexes: &[CatalogIndex],
) -> Result<Vec<TableSpec>, PythonMigrationError> {
    let mut stmt = conn.prepare("SELECT name, columns_json FROM _catalog_tables ORDER BY name")?;
    let rows = stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut specs = Vec::new();
    for row in rows {
        let (name, columns_json) = row?;
        let columns: Vec<PythonColumnDef> = serde_json::from_str(&columns_json)?;
        let rust_columns = columns
            .iter()
            .map(column_to_rust)
            .collect::<Result<Vec<_>, _>>()?;
        let fts_fields = infer_fts_fields(conn, &name, indexes)?;
        let vector_fields = infer_vector_fields(&name, &columns, indexes)?;
        specs.push(TableSpec {
            name,
            columns,
            rust_columns,
            fts_fields,
            vector_fields,
        });
    }
    Ok(specs)
}

pub(super) fn infer_fts_fields(
    conn: &Connection,
    table: &str,
    indexes: &[CatalogIndex],
) -> Result<Vec<String>, PythonMigrationError> {
    let mut fields = BTreeSet::new();
    for idx in indexes
        .iter()
        .filter(|idx| idx.table_name == table && idx.index_type.eq_ignore_ascii_case("gin"))
    {
        for col in &idx.columns {
            fields.insert(col.clone());
        }
    }

    let prefix = format!("_inverted_{table}_");
    let mut stmt = conn.prepare(
        "SELECT name FROM sqlite_master WHERE type = 'table' AND name LIKE ?1 ORDER BY name",
    )?;
    let rows = stmt.query_map([format!("{prefix}%")], |row| row.get::<_, String>(0))?;
    for row in rows {
        let name = row?;
        if let Some(field) = name.strip_prefix(&prefix) {
            fields.insert(field.to_string());
        }
    }
    Ok(fields.into_iter().collect())
}

pub(super) fn infer_vector_fields(
    table: &str,
    columns: &[PythonColumnDef],
    indexes: &[CatalogIndex],
) -> Result<Vec<VectorSpec>, PythonMigrationError> {
    let mut params_by_field = BTreeMap::new();
    for idx in indexes.iter().filter(|idx| {
        idx.table_name == table
            && (idx.index_type.eq_ignore_ascii_case("ivf")
                || idx.index_type.eq_ignore_ascii_case("hnsw"))
    }) {
        for col in &idx.columns {
            let params = IVFIndexParams::from_catalog_map(&idx.parameters).map_err(|error| {
                PythonMigrationError::Invalid(format!(
                    "invalid persisted {} index `{}` parameters for {table}.{col}: {error}",
                    idx.index_type, idx.name
                ))
            })?;
            params_by_field.insert(col.clone(), params);
        }
    }

    let mut specs = Vec::new();
    for col in columns {
        let dimensions = match col.vector_dimensions {
            Some(dim) if dim > 0 => Some(dim),
            _ if col.type_name.eq_ignore_ascii_case("vector") => Some(0),
            _ => None,
        };
        if let Some(dimensions) = dimensions {
            specs.push(VectorSpec {
                field: col.name.clone(),
                dimensions,
                params: params_by_field
                    .remove(&col.name)
                    .unwrap_or_else(IVFIndexParams::default),
            });
        }
    }
    Ok(specs)
}

pub(super) fn column_to_rust(col: &PythonColumnDef) -> Result<ColumnDef, PythonMigrationError> {
    let ty = rust_column_type(col)?;
    Ok(ColumnDef {
        name: col.name.clone(),
        ty,
        primary_key: col.primary_key,
        not_null: col.not_null,
        auto_increment: col.auto_increment,
        unique: col.unique,
        default: col
            .default
            .as_ref()
            .filter(|value| !value.is_null())
            .map(json_to_value)
            .transpose()?
            .map(Expr::Literal),
        check: None,
        references: None,
    })
}

pub(super) fn rust_column_type(col: &PythonColumnDef) -> Result<ColumnType, PythonMigrationError> {
    let raw = col.type_name.to_ascii_lowercase();
    let mut scalar_name = raw.trim();
    let mut dimensions = 0_usize;
    while let Some(element_name) = scalar_name.strip_suffix("[]") {
        dimensions = dimensions.checked_add(1).ok_or_else(|| {
            PythonMigrationError::Invalid(format!(
                "array dimension count for column {} exceeds the supported range",
                col.name
            ))
        })?;
        scalar_name = element_name.trim_end();
    }
    if scalar_name.is_empty() {
        return Err(PythonMigrationError::Invalid(format!(
            "array column {} is missing its element type",
            col.name
        )));
    }
    let mut ty = rust_scalar_column_type(col, scalar_name)?;
    for _ in 0..dimensions {
        ty = ColumnType::Array(Box::new(ty));
    }
    Ok(ty)
}

pub(super) fn rust_scalar_column_type(
    col: &PythonColumnDef,
    raw: &str,
) -> Result<ColumnType, PythonMigrationError> {
    if raw == "vector" {
        let Some(dim) = col.vector_dimensions else {
            return Err(PythonMigrationError::Invalid(format!(
                "VECTOR column {} is missing vector_dimensions",
                col.name
            )));
        };
        return Ok(ColumnType::Vector(dim));
    }
    if raw == "point" {
        return Ok(ColumnType::Json);
    }
    if let Some(ty) = python_temporal_type(raw) {
        return Ok(ty);
    }
    if is_python_text_type(raw) {
        return Ok(ColumnType::Text);
    }
    match raw {
        "integer" | "int" | "int2" | "int4" | "int8" | "bigint" | "smallint" | "serial"
        | "bigserial" | "serial4" | "serial8" => Ok(ColumnType::Integer),
        "bool" | "boolean" => Ok(ColumnType::Boolean),
        "real" | "float" | "float4" | "float8" | "double precision" => Ok(ColumnType::Real),
        "numeric" | "decimal" => {
            let scale = col
                .numeric_scale
                .map(|scale| {
                    i32::try_from(scale).map_err(|_| {
                        PythonMigrationError::Invalid(format!(
                            "numeric scale {scale} for column {} exceeds the supported i32 range",
                            col.name
                        ))
                    })
                })
                .transpose()?
                .or(col.numeric_precision.map(|_| 0));
            Ok(ColumnType::Numeric {
                precision: col.numeric_precision,
                scale,
            })
        }
        "json" => Ok(ColumnType::Json),
        "jsonb" => Ok(ColumnType::JsonB),
        "bytea" => Ok(ColumnType::Bytea),
        _ => Ok(ColumnType::Text),
    }
}

pub(super) fn is_python_text_type(raw: &str) -> bool {
    matches!(
        raw,
        "text" | "varchar" | "character varying" | "char" | "character" | "name" | "uuid"
    )
}

pub(super) fn python_temporal_type(raw: &str) -> Option<ColumnType> {
    match raw {
        "date" => Some(ColumnType::Date),
        "time" | "time without time zone" => Some(ColumnType::Time),
        "timetz" | "time with time zone" => Some(ColumnType::TimeTz),
        "datetime" | "timestamp" | "timestamp without time zone" => Some(ColumnType::Timestamp),
        "timestamptz" | "timestamp with time zone" => Some(ColumnType::TimestampTz),
        _ => None,
    }
}
