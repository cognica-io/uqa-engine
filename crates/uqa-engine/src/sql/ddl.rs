//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL DDL execution and column type conversion helpers.

use super::{
    eval, index_vectors_for_type, run_select, value_to_tensor, value_to_vector, AlterTableAction,
    AlterTableStmt, BTreeMap, ColumnType, CreateIndex, CreateTable, Document, DropKind, DropStmt,
    Engine, EvalContext, IVFIndexParams, RowUpdateVectors, SQLColumnDef, SQLError, SQLParam,
    SQLResult, TemporalValue, Value,
};

pub(super) fn run_create_sequence(
    engine: &Engine,
    s: uqa_sql::ast::CreateSequence,
) -> Result<SQLResult, SQLError> {
    if !engine.create_sequence(&s.name, s.start, s.increment, s.if_not_exists) {
        return Err(SQLError::Unsupported(format!(
            "Sequence `{}` already exists",
            s.name
        )));
    }
    Ok(SQLResult::empty())
}

pub(super) fn run_alter_sequence(
    engine: &Engine,
    s: uqa_sql::ast::AlterSequence,
) -> Result<SQLResult, SQLError> {
    engine
        .alter_sequence(&s.name, s.restart, s.increment, s.start)
        .map_err(SQLError::Unsupported)?;
    Ok(SQLResult::empty())
}

pub(super) fn run_create_table_as(
    engine: &Engine,
    name: String,
    if_not_exists: bool,
    body: uqa_sql::ast::SelectStmt,
    params: &[SQLParam],
) -> Result<SQLResult, SQLError> {
    if engine.table(&name).is_some() {
        if if_not_exists {
            return Ok(SQLResult::empty());
        }
        return Err(SQLError::Unsupported(format!(
            "Table `{name}` already exists"
        )));
    }
    let result = run_select(engine, body, params)?;
    let cols: Vec<uqa_sql::ast::ColumnDef> = result
        .columns
        .iter()
        .map(|c| uqa_sql::ast::ColumnDef {
            name: c.clone(),
            ty: uqa_sql::ast::ColumnType::Text,
            primary_key: false,
            not_null: false,
            auto_increment: false,
            unique: false,
            default: None,
            check: None,
            references: None,
        })
        .collect();
    let analyzer = uqa_analysis::analyzer::standard_analyzer("english");
    engine.create_table(name.clone(), analyzer, Vec::new());
    if let Some(t) = engine.table(&name) {
        (*t.columns.write()).clone_from(&cols);
    }
    let mut affected: u64 = 0;
    for (idx, row) in result.rows.iter().enumerate() {
        let doc_id = (idx as u64) + 1;
        let mut document = Document::new();
        for (k, v) in row {
            document.insert(k.clone(), v.clone());
        }
        engine.add_document(&name, doc_id, document);
        affected += 1;
    }
    Ok(SQLResult::from_affected(affected))
}

pub(super) fn run_drop(engine: &Engine, stmt: DropStmt) -> Result<SQLResult, SQLError> {
    match stmt.kind {
        DropKind::Table => {
            for name in &stmt.names {
                if !engine.has_table(name) {
                    if stmt.if_exists {
                        continue;
                    }
                    return Err(SQLError::Unsupported(format!(
                        "DROP TABLE: relation `{name}` does not exist"
                    )));
                }
                let referrers = engine.referrers_to(name);
                if !referrers.is_empty() {
                    if stmt.cascade {
                        // CASCADE: drop every referrer first. The
                        // recursive walk catches transitive
                        // dependencies (A -> B -> C).
                        let referrer_names: Vec<String> =
                            referrers.iter().map(|(n, _)| n.clone()).collect();
                        let mut queue: Vec<String> = referrer_names;
                        while let Some(other) = queue.pop() {
                            for (next, _) in engine.referrers_to(&other) {
                                queue.push(next);
                            }
                            engine.drop_table(&other);
                        }
                    } else {
                        let names: Vec<String> = referrers.iter().map(|(n, _)| n.clone()).collect();
                        return Err(SQLError::TypeMismatch(format!(
                            "DROP TABLE `{name}` rejected: still referenced by `{}`. Use CASCADE.",
                            names.join(", ")
                        )));
                    }
                }
                engine.drop_table(name);
            }
        }
        DropKind::Index => {
            // Persisted as `_catalog_indexes` rows. The in-memory
            // physical structures (FTS / vector indexes attached to
            // table fields) are not torn down here -- the catalog
            // entry merely tracks the CREATE INDEX statement so it
            // survives Engine::open.
            for name in &stmt.names {
                engine.drop_catalog_index(name);
            }
        }
        DropKind::View => {
            for name in &stmt.names {
                if !engine.drop_view(name) && !stmt.if_exists {
                    return Err(SQLError::Unsupported(format!(
                        "DROP VIEW: relation `{name}` does not exist"
                    )));
                }
            }
        }
        DropKind::Schema => {
            for name in &stmt.names {
                if !engine.drop_schema(name) && !stmt.if_exists {
                    return Err(SQLError::Unsupported(format!(
                        "DROP SCHEMA: schema `{name}` does not exist"
                    )));
                }
            }
        }
    }
    Ok(SQLResult::empty())
}

pub(super) fn run_alter_table(
    engine: &Engine,
    stmt: AlterTableStmt,
) -> Result<SQLResult, SQLError> {
    if !engine.has_table(&stmt.table) {
        if stmt.if_exists {
            return Ok(SQLResult::empty());
        }
        return Err(SQLError::Unsupported(format!(
            "ALTER TABLE: relation `{}` does not exist",
            stmt.table
        )));
    }
    match stmt.action {
        AlterTableAction::AddColumn {
            column,
            if_not_exists,
        } => {
            let col_name = column.name.clone();
            if engine.table_has_column(&stmt.table, &col_name) {
                if if_not_exists {
                    return Ok(SQLResult::empty());
                }
                return Err(SQLError::Unsupported(format!(
                    "ALTER TABLE ADD COLUMN: column `{col_name}` already exists"
                )));
            }
            match column.ty {
                ColumnType::Vector(dim) | ColumnType::Tensor(dim) => {
                    engine.create_vector_field(&stmt.table, col_name.clone(), dim);
                }
                ColumnType::Text => {
                    if let Err(e) = engine.add_fts_field(&stmt.table, col_name.clone()) {
                        return Err(SQLError::Internal(format!("add_fts_field: {e}")));
                    }
                }
                _ => {}
            }
            // Capture the default expression and NOT NULL flag before
            // moving the column into the engine so we can backfill any
            // existing rows. PostgreSQL evaluates the default once per
            // existing row at ALTER TABLE time, which keeps NOT NULL
            // constraints satisfiable for non-empty tables.
            let default_expr = column.default.clone();
            let column_not_null = column.not_null;
            engine.register_column(&stmt.table, column);
            backfill_added_column(
                engine,
                &stmt.table,
                &col_name,
                default_expr.as_ref(),
                column_not_null,
            )?;
        }
        AlterTableAction::DropColumn {
            name,
            if_exists,
            cascade: _,
        } => {
            if !engine.table_has_column(&stmt.table, &name) {
                if if_exists {
                    return Ok(SQLResult::empty());
                }
                return Err(SQLError::Unsupported(format!(
                    "ALTER TABLE DROP COLUMN: column `{name}` does not exist"
                )));
            }
            engine.drop_column(&stmt.table, &name);
        }
        AlterTableAction::RenameColumn { from, to } => {
            if !engine.table_has_column(&stmt.table, &from) {
                return Err(SQLError::Unsupported(format!(
                    "ALTER TABLE RENAME COLUMN: column `{from}` does not exist"
                )));
            }
            if engine.table_has_column(&stmt.table, &to) {
                return Err(SQLError::Unsupported(format!(
                    "ALTER TABLE RENAME COLUMN: column `{to}` already exists"
                )));
            }
            engine.rename_column(&stmt.table, &from, &to);
        }
        AlterTableAction::RenameTable { to } => {
            if engine.has_table(&to) {
                return Err(SQLError::Unsupported(format!(
                    "ALTER TABLE RENAME: relation `{to}` already exists"
                )));
            }
            if !engine.rename_table(&stmt.table, &to) {
                return Err(SQLError::Unsupported(format!(
                    "ALTER TABLE RENAME: rename of `{}` failed",
                    stmt.table
                )));
            }
        }
        AlterTableAction::SetDefault { name, default } => {
            if !engine.set_column_default(&stmt.table, &name, Some(default)) {
                return Err(SQLError::Unsupported(format!(
                    "ALTER TABLE ALTER COLUMN: column `{name}` does not exist"
                )));
            }
        }
        AlterTableAction::DropDefault { name } => {
            if !engine.set_column_default(&stmt.table, &name, None) {
                return Err(SQLError::Unsupported(format!(
                    "ALTER TABLE ALTER COLUMN: column `{name}` does not exist"
                )));
            }
        }
        AlterTableAction::SetNotNull { name } => {
            ensure_column_exists(engine, &stmt.table, &name)?;
            ensure_existing_values_not_null(engine, &stmt.table, &name)?;
            engine.set_column_not_null(&stmt.table, &name, true);
        }
        AlterTableAction::DropNotNull { name } => {
            if !engine.set_column_not_null(&stmt.table, &name, false) {
                return Err(SQLError::Unsupported(format!(
                    "ALTER TABLE ALTER COLUMN: column `{name}` does not exist"
                )));
            }
        }
        AlterTableAction::AlterColumnType { name, ty } => {
            ensure_column_exists(engine, &stmt.table, &name)?;
            rewrite_column_values_to_type(engine, &stmt.table, &name, &ty)?;
            engine.set_column_type(&stmt.table, &name, &ty);
            match ty {
                ColumnType::Text => {
                    if let Err(e) = engine.add_fts_field(&stmt.table, name.clone()) {
                        return Err(SQLError::Internal(format!("add_fts_field: {e}")));
                    }
                }
                ColumnType::Vector(dim) | ColumnType::Tensor(dim) => {
                    engine.create_vector_field(&stmt.table, name, dim);
                }
                _ => {}
            }
        }
    }
    Ok(SQLResult::empty())
}

fn ensure_column_exists(engine: &Engine, table: &str, column: &str) -> Result<(), SQLError> {
    if engine.table_has_column(table, column) {
        Ok(())
    } else {
        Err(SQLError::Unsupported(format!(
            "ALTER TABLE ALTER COLUMN: column `{column}` does not exist"
        )))
    }
}

fn ensure_existing_values_not_null(
    engine: &Engine,
    table: &str,
    column: &str,
) -> Result<(), SQLError> {
    let mut null_rows = 0usize;
    for doc_id in engine.table_doc_ids(table) {
        let Some(doc) = engine.get_document(table, doc_id) else {
            continue;
        };
        if matches!(doc.get(column), None | Some(Value::Null)) {
            null_rows += 1;
        }
    }
    if null_rows > 0 {
        return Err(SQLError::TypeMismatch(format!(
            "ALTER TABLE ALTER COLUMN: column `{column}` contains NULL values"
        )));
    }
    Ok(())
}

/// Coerce a write value to fit the column's declared type.
pub(super) fn coerce_to_column_type(
    engine: &Engine,
    table: &str,
    column: &str,
    value: Value,
) -> Result<Value, SQLError> {
    let cols = match engine.describe_table(table) {
        Some(c) => c,
        None => return Ok(value),
    };
    let Some(def) = cols.iter().find(|c| c.name == column) else {
        return Ok(value);
    };
    if let ColumnType::Numeric { scale: Some(s), .. } = &def.ty {
        return Ok(round_numeric(value, *s));
    }
    if matches!(&def.ty, ColumnType::Real) {
        return match value {
            Value::Float(_) => Ok(value),
            Value::Int(i) => Ok(Value::Float(i as f64)),
            Value::Str(s) => Ok(s.parse::<f64>().map(Value::Float).unwrap_or(Value::Str(s))),
            other => Ok(other),
        };
    }
    if matches!(&def.ty, ColumnType::Json) {
        return Ok(coerce_json_value(value));
    }
    if matches!(&def.ty, ColumnType::Bytea) {
        return match value {
            Value::Bytes(_) => Ok(value),
            Value::Str(s) => Ok(Value::Bytes(s.into_bytes())),
            other => Ok(other),
        };
    }
    if matches!(&def.ty, ColumnType::Vector(_) | ColumnType::Tensor(_)) {
        return convert_value_to_column_type(value, &def.ty);
    }
    if is_temporal_column_type(&def.ty) {
        return convert_value_to_column_type(value, &def.ty);
    }
    Ok(value)
}

fn coerce_json_value(value: Value) -> Value {
    match value {
        Value::Str(s) => serde_json::from_str::<serde_json::Value>(&s)
            .map(json_to_core_value)
            .unwrap_or(Value::Str(s)),
        other => other,
    }
}

fn rewrite_column_values_to_type(
    engine: &Engine,
    table: &str,
    column: &str,
    ty: &ColumnType,
) -> Result<(), SQLError> {
    for doc_id in engine.table_doc_ids(table) {
        let Some(doc) = engine.get_document(table, doc_id) else {
            continue;
        };
        let Some(value) = doc.get(column).cloned() else {
            continue;
        };
        let converted = convert_value_to_column_type(value, ty)?;
        let mut updates: BTreeMap<String, Value> = BTreeMap::new();
        updates.insert(column.to_string(), converted.clone());
        let mut vectors: RowUpdateVectors = BTreeMap::new();
        if let Ok(values) = index_vectors_for_type(&converted, ty) {
            vectors.insert(column.to_string(), values);
        }
        engine.update_document_fields_with_vector_values(table, doc_id, updates, vectors);
    }
    Ok(())
}

fn convert_value_to_column_type(value: Value, ty: &ColumnType) -> Result<Value, SQLError> {
    if matches!(value, Value::Null) {
        return Ok(Value::Null);
    }
    match ty {
        ColumnType::Integer => match value {
            Value::Int(_) => Ok(value),
            Value::Float(f) => Ok(Value::Int(f as i64)),
            Value::Bool(b) => Ok(Value::Int(i64::from(b))),
            Value::Str(s) => s
                .parse::<i64>()
                .map(Value::Int)
                .map_err(|e| SQLError::TypeMismatch(format!("cannot cast `{s}` to integer: {e}"))),
            other => Err(SQLError::TypeMismatch(format!(
                "cannot cast {other:?} to integer"
            ))),
        },
        ColumnType::Text => Ok(Value::Str(value_to_text(&value))),
        ColumnType::Real | ColumnType::Numeric { .. } => match value {
            Value::Float(_) => Ok(value),
            Value::Int(i) => Ok(Value::Float(i as f64)),
            Value::Bool(b) => Ok(Value::Float(if b { 1.0 } else { 0.0 })),
            Value::Str(s) => s
                .parse::<f64>()
                .map(Value::Float)
                .map_err(|e| SQLError::TypeMismatch(format!("cannot cast `{s}` to real: {e}"))),
            other => Err(SQLError::TypeMismatch(format!(
                "cannot cast {other:?} to real"
            ))),
        },
        ColumnType::Json => Ok(coerce_json_value(value)),
        ColumnType::Bytea => Ok(match value {
            Value::Bytes(_) => value,
            Value::Str(s) => Value::Bytes(s.into_bytes()),
            other => Value::Bytes(value_to_text(&other).into_bytes()),
        }),
        ColumnType::Date
        | ColumnType::Time
        | ColumnType::TimeTz
        | ColumnType::Timestamp
        | ColumnType::TimestampTz => convert_temporal_value(value, ty),
        ColumnType::Vector(dim) => {
            let vector = value_to_vector(&value)?;
            validate_vector_dimensions(*dim, vector.len())?;
            Ok(value)
        }
        ColumnType::Tensor(dim) => {
            let tensor = value_to_tensor(&value)?;
            for vector in &tensor {
                validate_vector_dimensions(*dim, vector.len())?;
            }
            Ok(value)
        }
    }
}

pub(super) fn validate_vector_dimensions(expected: u32, actual: usize) -> Result<(), SQLError> {
    let expected = expected as usize;
    if actual == expected {
        Ok(())
    } else {
        Err(SQLError::VectorDimMismatch { expected, actual })
    }
}

fn is_temporal_column_type(ty: &ColumnType) -> bool {
    matches!(
        ty,
        ColumnType::Date
            | ColumnType::Time
            | ColumnType::TimeTz
            | ColumnType::Timestamp
            | ColumnType::TimestampTz
    )
}

pub(super) fn column_type_name(ty: &ColumnType) -> &'static str {
    match ty {
        ColumnType::Integer => "integer",
        ColumnType::Text => "text",
        ColumnType::Real => "real",
        ColumnType::Numeric { .. } => "numeric",
        ColumnType::Json => "json",
        ColumnType::Bytea => "bytea",
        ColumnType::Date => "date",
        ColumnType::Time => "time",
        ColumnType::TimeTz => "time with time zone",
        ColumnType::Timestamp => "timestamp",
        ColumnType::TimestampTz => "timestamp with time zone",
        ColumnType::Vector(_) => "vector",
        ColumnType::Tensor(_) => "tensor",
    }
}

fn convert_temporal_value(value: Value, ty: &ColumnType) -> Result<Value, SQLError> {
    match value {
        Value::Temporal(temporal) => Ok(Value::Temporal(temporal)),
        other => {
            let text = value_to_text(&other);
            let parsed = match ty {
                ColumnType::Date => TemporalValue::parse_date(&text),
                ColumnType::Time => TemporalValue::parse_time(&text),
                ColumnType::TimeTz => TemporalValue::parse_time_tz(&text),
                ColumnType::Timestamp => TemporalValue::parse_timestamp(&text),
                ColumnType::TimestampTz => TemporalValue::parse_timestamp_tz(&text),
                _ => None,
            };
            parsed.map(Value::Temporal).ok_or_else(|| {
                SQLError::TypeMismatch(format!("cannot cast `{text}` to {}", column_type_name(ty)))
            })
        }
    }
}

pub(super) fn value_to_text(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::Bool(b) => b.to_string(),
        Value::Int(i) => i.to_string(),
        Value::Float(f) => f.to_string(),
        Value::Str(s) => s.clone(),
        Value::Bytes(bytes) => String::from_utf8_lossy(bytes).into_owned(),
        Value::Temporal(t) => t.to_sql_string(),
        Value::List(_) | Value::Map(_) => serde_json::to_string(&core_value_to_json(value))
            .unwrap_or_else(|_| format!("{value:?}")),
    }
}

pub(super) fn json_to_core_value(json: serde_json::Value) -> Value {
    match json {
        serde_json::Value::Null => Value::Null,
        serde_json::Value::Bool(b) => Value::Bool(b),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Value::Int(i)
            } else if let Some(f) = n.as_f64() {
                Value::Float(f)
            } else {
                Value::Null
            }
        }
        serde_json::Value::String(s) => Value::Str(s),
        serde_json::Value::Array(items) => {
            Value::List(items.into_iter().map(json_to_core_value).collect())
        }
        serde_json::Value::Object(obj) => {
            if let Ok(temporal) =
                serde_json::from_value::<TemporalValue>(serde_json::Value::Object(obj.clone()))
            {
                return Value::Temporal(temporal);
            }
            Value::Map(
                obj.into_iter()
                    .map(|(k, v)| (k, json_to_core_value(v)))
                    .collect(),
            )
        }
    }
}

pub(super) fn core_value_to_json(value: &Value) -> serde_json::Value {
    match value {
        Value::Null => serde_json::Value::Null,
        Value::Bool(b) => serde_json::Value::Bool(*b),
        Value::Int(i) => serde_json::Value::Number((*i).into()),
        Value::Float(f) => serde_json::Number::from_f64(*f)
            .map_or(serde_json::Value::Null, serde_json::Value::Number),
        Value::Str(s) => serde_json::from_str::<serde_json::Value>(s)
            .unwrap_or_else(|_| serde_json::Value::String(s.clone())),
        Value::Bytes(bytes) => serde_json::Value::String(String::from_utf8_lossy(bytes).into()),
        Value::Temporal(t) => serde_json::Value::String(t.to_sql_string()),
        Value::List(items) => {
            serde_json::Value::Array(items.iter().map(core_value_to_json).collect())
        }
        Value::Map(map) => serde_json::Value::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), core_value_to_json(v)))
                .collect(),
        ),
    }
}

pub(super) fn json_table_value_to_text(value: &serde_json::Value) -> Value {
    match value {
        serde_json::Value::Null => Value::Null,
        serde_json::Value::String(s) => Value::Str(s.clone()),
        serde_json::Value::Bool(b) => Value::Str(b.to_string()),
        serde_json::Value::Number(n) => Value::Str(n.to_string()),
        serde_json::Value::Array(_) | serde_json::Value::Object(_) => {
            Value::Str(serde_json::to_string(value).unwrap_or_default())
        }
    }
}

pub(super) fn json_table_arg(value: &Value, name: &str) -> Result<serde_json::Value, SQLError> {
    match value {
        Value::Str(s) => serde_json::from_str::<serde_json::Value>(s)
            .map_err(|e| SQLError::TypeMismatch(format!("{name}: invalid JSON: {e}"))),
        other => Ok(core_value_to_json(other)),
    }
}

fn round_numeric(value: Value, scale: u32) -> Value {
    let factor = 10f64.powi(scale as i32);
    match value {
        Value::Float(f) => {
            let rounded = (f * factor).round() / factor;
            Value::Float(rounded)
        }
        Value::Int(i) => Value::Float(i as f64),
        other => other,
    }
}

/// Apply the new column's DEFAULT (or NULL) value to every row that
/// existed before the ADD COLUMN. `PostgreSQL` evaluates the default
/// once per existing row at ALTER TABLE time so NOT NULL columns stay
/// consistent on non-empty tables; the UQA-RS implementation mirrors that
/// semantics by sweeping the document store.
fn backfill_added_column(
    engine: &Engine,
    table: &str,
    column: &str,
    default_expr: Option<&uqa_sql::ast::Expr>,
    not_null: bool,
) -> Result<(), SQLError> {
    let doc_ids = engine.table_doc_ids(table);
    if doc_ids.is_empty() {
        return Ok(());
    }
    let default_value = if let Some(expr) = default_expr {
        let ctx = EvalContext::new(None, &[]).with_engine(engine);
        eval(expr, &ctx)?
    } else if not_null {
        return Err(SQLError::TypeMismatch(format!(
            "ALTER TABLE ADD COLUMN `{column}` is NOT NULL but no DEFAULT supplied; \
             {} existing row(s) would violate the constraint",
            doc_ids.len()
        )));
    } else {
        Value::Null
    };
    let default_value = coerce_to_column_type(engine, table, column, default_value)?;
    let vector_value: Option<Vec<Vec<f32>>> = engine
        .column_type(table, column)
        .and_then(|ty| index_vectors_for_type(&default_value, &ty).ok());
    for doc_id in doc_ids {
        let mut updates: BTreeMap<String, Value> = BTreeMap::new();
        updates.insert(column.to_string(), default_value.clone());
        let mut vectors: RowUpdateVectors = BTreeMap::new();
        if let Some(v) = vector_value.as_ref() {
            vectors.insert(column.to_string(), v.clone());
        }
        engine.update_document_fields_with_vector_values(table, doc_id, updates, vectors);
    }
    Ok(())
}

// DDL
// -------------------------------------------------------------------------

pub(super) fn run_create_table(engine: &Engine, c: CreateTable) -> Result<SQLResult, SQLError> {
    if engine.has_table(&c.name) {
        if c.if_not_exists {
            return Ok(SQLResult::empty());
        }
        return Err(SQLError::Unsupported(format!(
            "CREATE TABLE: relation `{}` already exists",
            c.name
        )));
    }
    let mut vector_fields: Vec<(String, u32)> = Vec::new();
    for col in &c.columns {
        match &col.ty {
            ColumnType::Vector(dim) | ColumnType::Tensor(dim) => {
                vector_fields.push((col.name.clone(), *dim));
            }
            _ => {}
        }
    }
    engine.create_default_table(c.name.clone(), Vec::new());
    for (field, dim) in vector_fields {
        engine.create_vector_field(&c.name, field, dim);
    }
    for col in &c.columns {
        engine.register_column(&c.name, col.clone());
    }
    engine.register_table_constraints(&c.name, c.checks.clone(), c.foreign_keys.clone());
    let _ = column_names(&c.columns);
    Ok(SQLResult::empty())
}

fn column_names(cols: &[SQLColumnDef]) -> Vec<String> {
    cols.iter().map(|c| c.name.clone()).collect()
}

pub(super) fn run_create_index(engine: &Engine, c: CreateIndex) -> Result<SQLResult, SQLError> {
    // CREATE INDEX is metadata-bearing now: `gin` registers the column
    // as an FTS field with the analyzer specified in `WITH (analyzer = ...)`,
    // `ivf` rebuilds the vector field with an IVF backend, `hnsw` is a
    // compatibility alias for the same backend, and others are informational.
    if let Some(name) = c.name.as_ref() {
        if engine.has_catalog_index(name) {
            if c.if_not_exists {
                return Ok(SQLResult::empty());
            }
            return Err(SQLError::Unsupported(format!(
                "Index `{name}` already exists"
            )));
        }
    }

    let am = c.access_method.to_ascii_lowercase();
    match am.as_str() {
        "gin" => {
            for col in &c.columns {
                let analyzer = c
                    .options
                    .iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case("analyzer"))
                    .map(|(_, v)| v.as_str());
                if let Err(e) = engine.add_fts_field_with_analyzer(&c.table, col.clone(), analyzer)
                {
                    return Err(SQLError::Internal(format!("add_fts_field: {e}")));
                }
            }
        }
        "" => {}
        "ivf" | "hnsw" => {
            let params = parse_ivf_index_params(&c.options)?;
            for col in &c.columns {
                match engine.column_type(&c.table, col) {
                    Some(ColumnType::Vector(dim) | ColumnType::Tensor(dim)) => {
                        if !engine.rebuild_ivf_vector_field(&c.table, col.clone(), dim, params) {
                            return Err(SQLError::Unsupported(format!(
                                "CREATE INDEX USING ivf: relation `{}` does not exist",
                                c.table
                            )));
                        }
                    }
                    Some(other) => {
                        return Err(SQLError::Unsupported(format!(
                            "CREATE INDEX USING ivf requires VECTOR or TENSOR column `{col}`, got {other:?}"
                        )));
                    }
                    None => {
                        return Err(SQLError::Unsupported(format!(
                            "CREATE INDEX USING ivf: column `{}`.`{col}` does not exist",
                            c.table
                        )));
                    }
                }
            }
        }
        _ => {}
    }
    // Persist the CREATE INDEX statement itself so reopen sees the
    // same set of registered indexes. The engine layer parses
    // `parameters_json` back into `(key, value)` pairs and re-runs
    // any access-method-specific side effects (e.g. add_fts_field
    // for `gin`) on restore.
    if let Some(name) = c.name.as_ref() {
        let catalog_index_type = match am.as_str() {
            "" => "btree",
            "hnsw" => "ivf",
            other => other,
        };
        engine.register_catalog_index(name, catalog_index_type, &c.table, &c.columns, &c.options);
    }
    Ok(SQLResult::empty())
}

fn parse_ivf_index_params(options: &[(String, String)]) -> Result<IVFIndexParams, SQLError> {
    let mut params = IVFIndexParams::default();
    for (key, value) in options {
        if key.eq_ignore_ascii_case("lists") || key.eq_ignore_ascii_case("nlist") {
            params.nlist = parse_positive_usize_option(key, value)?;
        } else if key.eq_ignore_ascii_case("probes") || key.eq_ignore_ascii_case("nprobe") {
            params.nprobe = parse_positive_usize_option(key, value)?;
        } else if key.eq_ignore_ascii_case("train_threshold")
            || key.eq_ignore_ascii_case("train-threshold")
            || key.eq_ignore_ascii_case("min_train")
        {
            params.train_threshold = parse_positive_usize_option(key, value)?;
        } else {
            return Err(SQLError::Unsupported(format!(
                "CREATE INDEX USING ivf option `{key}` is not supported"
            )));
        }
    }
    Ok(params)
}

fn parse_positive_usize_option(key: &str, value: &str) -> Result<usize, SQLError> {
    let parsed = value.parse::<usize>().map_err(|_| {
        SQLError::TypeMismatch(format!(
            "CREATE INDEX USING ivf option `{key}` must be a positive integer"
        ))
    })?;
    if parsed == 0 {
        return Err(SQLError::TypeMismatch(format!(
            "CREATE INDEX USING ivf option `{key}` must be a positive integer"
        )));
    }
    Ok(parsed)
}
