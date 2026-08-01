//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Shared `PostgreSQL` catalog identifiers, type mappings, and row constructors.

use super::{
    canonical_routine_type_name, column_type_name, ColumnType, Engine, Expr, RelationIdentity,
    ResultRow, SQLColumnDef, SQLError, Value,
};

pub(super) fn catalog_name() -> Value {
    Value::Str("uqa".into())
}

pub(super) fn catalog_usize(value: usize, label: &str) -> Result<i64, SQLError> {
    i64::try_from(value).map_err(|_| {
        SQLError::Internal(format!(
            "{label} exceeds the SQL catalog BIGINT representation"
        ))
    })
}

pub(super) fn catalog_ordinal(index: usize, label: &str) -> Result<i64, SQLError> {
    let ordinal = index
        .checked_add(1)
        .ok_or_else(|| SQLError::Internal(format!("{label} ordinal overflow")))?;
    catalog_usize(ordinal, label)
}

pub(super) fn str_value(value: impl Into<String>) -> Value {
    Value::Str(value.into())
}

pub(super) fn int_value(value: i64) -> Value {
    Value::Int(value)
}

pub(super) fn bool_value(value: bool) -> Value {
    Value::Bool(value)
}

pub(super) fn list_int(values: &[i64]) -> Value {
    Value::List(values.iter().copied().map(Value::Int).collect())
}

pub(super) fn row(entries: impl IntoIterator<Item = (&'static str, Value)>) -> ResultRow {
    let mut out = ResultRow::new();
    for (key, value) in entries {
        out.insert(key.to_string(), value);
    }
    out
}

pub(super) fn split_schema_name(name: &str) -> Result<(String, String), SQLError> {
    let relation = RelationIdentity::from_legacy_name(name).map_err(|error| {
        SQLError::Internal(format!("invalid catalog relation `{name}`: {error}"))
    })?;
    Ok((relation.schema, relation.name))
}

pub(super) fn split_index_name(
    index_name: &str,
    table_schema: &str,
) -> Result<(String, String), SQLError> {
    let (schema, name) = RelationIdentity::parse_reference(index_name).map_err(|error| {
        SQLError::Internal(format!("invalid catalog index `{index_name}`: {error}"))
    })?;
    Ok((schema.unwrap_or_else(|| table_schema.to_string()), name))
}

pub(super) fn stable_oid(kind: &str, name: &str) -> i64 {
    let mut hash = 14_695_981_039_346_656_037_u64;
    for byte in kind.bytes().chain(*b":").chain(name.bytes()) {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(1_099_511_628_211);
    }
    10_000 + i64::try_from(hash % 2_000_000_000).unwrap_or(0)
}

pub(super) fn schema_oid(schema: &str) -> i64 {
    match schema {
        "pg_catalog" => 11,
        "public" => 2200,
        "information_schema" => 13_377,
        other => stable_oid("namespace", other),
    }
}

pub(super) fn relation_oid(kind: &str, schema: &str, name: &str) -> i64 {
    stable_oid(kind, &format!("{schema}.{name}"))
}

pub(super) fn current_user_oid() -> i64 {
    10
}

pub(super) fn current_user_name() -> &'static str {
    "uqa"
}

pub(super) fn all_schema_names(engine: &Engine) -> Result<Vec<String>, SQLError> {
    let mut schemas = vec!["pg_catalog".to_string(), "information_schema".to_string()];
    schemas.extend(
        engine
            .list_schemas()
            .map_err(|err| SQLError::Internal(format!("read schema catalog: {err}")))?,
    );
    schemas.sort();
    schemas.dedup();
    Ok(schemas)
}

pub(super) fn table_columns_for(
    engine: &Engine,
    table: &str,
) -> Result<Vec<SQLColumnDef>, SQLError> {
    Ok(engine
        .describe_table(table)
        .map_err(|err| SQLError::Internal(format!("read table schema: {err}")))?
        .unwrap_or_default())
}

pub(super) fn pg_type_oid(ty: &ColumnType) -> i64 {
    match ty {
        ColumnType::Integer => 23,
        ColumnType::Boolean => 16,
        ColumnType::Text => 25,
        ColumnType::Real => 701,
        ColumnType::Numeric { .. } => 1700,
        ColumnType::Json => 114,
        ColumnType::JsonB => 3802,
        ColumnType::Bytea => 17,
        ColumnType::Array(element) => match element.as_ref() {
            ColumnType::Integer => 1007,
            ColumnType::Boolean => 1000,
            ColumnType::Text => 1009,
            ColumnType::Real => 1022,
            ColumnType::Numeric { .. } => 1231,
            ColumnType::Json => 199,
            ColumnType::JsonB => 3807,
            ColumnType::Bytea => 1001,
            ColumnType::Date => 1182,
            ColumnType::Time => 1183,
            ColumnType::TimeTz => 1270,
            ColumnType::Timestamp => 1115,
            ColumnType::TimestampTz => 1185,
            ColumnType::Vector(_) => 380_002,
            ColumnType::Tensor(_) => 380_003,
            ColumnType::Array(_) => pg_type_oid(element),
        },
        ColumnType::Date => 1082,
        ColumnType::Time => 1083,
        ColumnType::TimeTz => 1266,
        ColumnType::Timestamp => 1114,
        ColumnType::TimestampTz => 1184,
        ColumnType::Vector(_) => 380_000,
        ColumnType::Tensor(_) => 380_001,
    }
}

pub(super) fn routine_type_oid(type_name: &str) -> i64 {
    let canonical = canonical_routine_type_name(type_name);
    match canonical.as_str() {
        "bool" => 16,
        "bytea" => 17,
        "int8" => 20,
        "int2" => 21,
        "int4" => 23,
        "text" => 25,
        "json" => 114,
        "float4" => 700,
        "float8" => 701,
        "varchar" => 1043,
        "date" => 1082,
        "time" => 1083,
        "timestamp" => 1114,
        "timestamptz" => 1184,
        "timetz" => 1266,
        "numeric" => 1700,
        "record" => 2249,
        "void" => 2278,
        "jsonb" => 3802,
        "vector" => 380_000,
        other => stable_oid("type", other),
    }
}

pub(super) fn pg_type_len(ty: &ColumnType) -> i64 {
    match ty {
        ColumnType::Integer => 4,
        ColumnType::Boolean => 1,
        ColumnType::Real | ColumnType::Timestamp | ColumnType::TimestampTz => 8,
        ColumnType::Date => 4,
        ColumnType::Time | ColumnType::TimeTz => 8,
        _ => -1,
    }
}

pub(super) fn info_datetime_precision(ty: &ColumnType) -> Value {
    match ty {
        ColumnType::Time | ColumnType::TimeTz | ColumnType::Timestamp | ColumnType::TimestampTz => {
            Value::Int(6)
        }
        _ => Value::Null,
    }
}

pub(super) fn info_numeric_precision(ty: &ColumnType) -> Value {
    match ty {
        ColumnType::Integer => Value::Int(32),
        ColumnType::Real => Value::Int(53),
        ColumnType::Numeric {
            precision: Some(precision),
            ..
        } => Value::Int(i64::from(*precision)),
        _ => Value::Null,
    }
}

pub(super) fn info_numeric_scale(ty: &ColumnType) -> Value {
    match ty {
        ColumnType::Numeric {
            scale: Some(scale), ..
        } => Value::Int(i64::from(*scale)),
        _ => Value::Null,
    }
}

pub(super) fn info_udt_name(ty: &ColumnType) -> &'static str {
    match ty {
        ColumnType::Integer => "int4",
        ColumnType::Boolean => "bool",
        ColumnType::Text => "text",
        ColumnType::Real => "float8",
        ColumnType::Numeric { .. } => "numeric",
        ColumnType::Json => "json",
        ColumnType::JsonB => "jsonb",
        ColumnType::Bytea => "bytea",
        ColumnType::Array(element) => match element.as_ref() {
            ColumnType::Integer => "_int4",
            ColumnType::Boolean => "_bool",
            ColumnType::Text => "_text",
            ColumnType::Real => "_float8",
            ColumnType::Numeric { .. } => "_numeric",
            ColumnType::Json => "_json",
            ColumnType::JsonB => "_jsonb",
            ColumnType::Bytea => "_bytea",
            ColumnType::Date => "_date",
            ColumnType::Time => "_time",
            ColumnType::TimeTz => "_timetz",
            ColumnType::Timestamp => "_timestamp",
            ColumnType::TimestampTz => "_timestamptz",
            ColumnType::Vector(_) => "_vector",
            ColumnType::Tensor(_) => "_tensor",
            ColumnType::Array(_) => info_udt_name(element),
        },
        ColumnType::Date => "date",
        ColumnType::Time => "time",
        ColumnType::TimeTz => "timetz",
        ColumnType::Timestamp => "timestamp",
        ColumnType::TimestampTz => "timestamptz",
        ColumnType::Vector(_) => "vector",
        ColumnType::Tensor(_) => "tensor",
    }
}

pub(super) fn info_data_type(ty: &ColumnType) -> &'static str {
    if matches!(ty, ColumnType::Array(_)) {
        "ARRAY"
    } else {
        column_type_name(ty)
    }
}

pub(super) fn array_dimension_count(ty: &ColumnType) -> i64 {
    let mut dimensions = 0_i64;
    let mut current = ty;
    while let ColumnType::Array(element) = current {
        dimensions += 1;
        current = element;
    }
    dimensions
}

pub(super) fn default_expr_text(expr: Option<&Expr>) -> Value {
    expr.map_or(Value::Null, |expr| Value::Str(format!("{expr:?}")))
}

pub(super) fn index_columns(columns_json: &str) -> Result<Vec<String>, SQLError> {
    serde_json::from_str(columns_json)
        .map_err(|err| SQLError::Internal(format!("decode index column catalog: {err}")))
}

pub(super) fn indexdef(name: &str, index_type: &str, table: &str, columns: &[String]) -> String {
    let method = if index_type.is_empty() {
        "btree"
    } else {
        index_type
    };
    format!(
        "CREATE INDEX {name} ON {table} USING {method} ({})",
        columns.join(", ")
    )
}

pub(super) type ColumnConstraintRow = (String, String, String, String, String, i64);

pub(super) fn column_constraint_rows(
    engine: &Engine,
) -> Result<Vec<ColumnConstraintRow>, SQLError> {
    let mut out = Vec::new();
    for table_name in engine
        .table_names()
        .map_err(|err| SQLError::Internal(format!("read table catalog: {err}")))?
    {
        let (schema, table) = split_schema_name(&table_name)?;
        for (idx, col) in table_columns_for(engine, &table_name)?.iter().enumerate() {
            let ordinal = i64::try_from(idx.checked_add(1).ok_or_else(|| {
                SQLError::Internal(format!(
                    "column ordinal overflow while reading catalog table `{table_name}`"
                ))
            })?)
            .map_err(|_| {
                SQLError::Internal(format!(
                    "column ordinal exceeds SQL BIGINT while reading catalog table `{table_name}`"
                ))
            })?;
            if col.primary_key {
                out.push((
                    schema.clone(),
                    table.clone(),
                    col.name.clone(),
                    format!("{table}_{}_pkey", col.name),
                    "PRIMARY KEY".to_string(),
                    ordinal,
                ));
            }
            if col.unique {
                out.push((
                    schema.clone(),
                    table.clone(),
                    col.name.clone(),
                    format!("{table}_{}_key", col.name),
                    "UNIQUE".to_string(),
                    ordinal,
                ));
            }
            if col.references.is_some() {
                out.push((
                    schema.clone(),
                    table.clone(),
                    col.name.clone(),
                    format!("{table}_{}_fkey", col.name),
                    "FOREIGN KEY".to_string(),
                    ordinal,
                ));
            }
            if col.check.is_some() {
                out.push((
                    schema.clone(),
                    table.clone(),
                    col.name.clone(),
                    format!("{table}_{}_check", col.name),
                    "CHECK".to_string(),
                    ordinal,
                ));
            }
        }
    }
    Ok(out)
}
