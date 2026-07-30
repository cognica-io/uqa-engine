//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL DDL execution and column type conversion helpers.

use super::scalar::eval_lowered_expression;
use super::{
    index_vectors_for_type, value_to_tensor, value_to_vector, AlterTableAction, AlterTableStmt,
    BTreeMap, ColumnType, CreateIndex, CreateTable, DecimalValue, Document, DropKind, DropStmt,
    Engine, IVFIndexParams, RowUpdateVectors, SQLError, SQLParam, SQLResult, TemporalValue, Value,
};
use crate::CatalogIndexRow;

pub(super) fn run_create_sequence(
    engine: &Engine,
    s: uqa_sql::ast::CreateSequence,
) -> Result<SQLResult, SQLError> {
    engine
        .create_sequence(&s.name, s.start, s.increment, s.if_not_exists)
        .map_err(SQLError::Unsupported)?;
    Ok(SQLResult::empty())
}

pub(super) fn run_alter_sequence(
    engine: &Engine,
    s: uqa_sql::ast::AlterSequence,
) -> Result<SQLResult, SQLError> {
    engine
        .alter_sequence_if_exists(&s.name, s.restart, s.increment, s.start, s.if_exists)
        .map_err(SQLError::Unsupported)?;
    Ok(SQLResult::empty())
}

pub(super) fn run_create_table_as(
    engine: &Engine,
    name: String,
    if_not_exists: bool,
    query: &uqa_planner::QueryPlan,
    params: &[SQLParam],
) -> Result<SQLResult, SQLError> {
    if engine
        .try_has_table(&name)
        .map_err(|err| ddl_storage_error("CREATE TABLE AS", err))?
    {
        if if_not_exists {
            return Ok(SQLResult::empty());
        }
        return Err(SQLError::Unsupported(format!(
            "Table `{name}` already exists"
        )));
    }
    let result = super::select::execute_query_plan(engine, query, params)?;
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
    engine
        .create_table(name.clone(), analyzer, Vec::new())
        .map_err(|err| ddl_storage_error("CREATE TABLE AS", err))?;
    if let Some(t) = engine
        .try_table(&name)
        .map_err(|err| ddl_storage_error("CREATE TABLE AS schema", err))?
    {
        (*t.columns.write()).clone_from(&cols);
    }
    engine
        .try_persist_table_schema(&name)
        .map_err(|err| ddl_storage_error("CREATE TABLE AS schema", err))?;
    let mut affected: u64 = 0;
    for (idx, row) in result.rows.iter().enumerate() {
        let doc_id = (idx as u64) + 1;
        let mut document = Document::new();
        for (k, v) in row {
            document.insert(k.clone(), v.clone());
        }
        engine.add_document(&name, doc_id, document)?;
        affected += 1;
    }
    Ok(SQLResult::from_affected(affected))
}

pub(super) fn run_drop(engine: &Engine, stmt: DropStmt) -> Result<SQLResult, SQLError> {
    if stmt.cascade && matches!(stmt.kind, DropKind::View | DropKind::Schema) {
        return Err(SQLError::Unsupported(format!(
            "DROP {} CASCADE is not supported; no objects were changed",
            match stmt.kind {
                DropKind::View => "VIEW",
                DropKind::Schema => "SCHEMA",
                DropKind::Table | DropKind::Index => unreachable!(),
            }
        )));
    }
    engine.with_implicit_transaction(move |engine| run_drop_inner(engine, stmt))
}

fn run_drop_inner(engine: &Engine, stmt: DropStmt) -> Result<SQLResult, SQLError> {
    match stmt.kind {
        DropKind::Table => {
            let mut tables = Vec::new();
            for name in &stmt.names {
                match engine
                    .try_resolve_relation_kind(name)
                    .map_err(|err| ddl_storage_error("DROP TABLE", err))?
                {
                    Some((canonical, "table")) => tables.push(canonical),
                    Some((canonical, kind)) => {
                        return Err(SQLError::Unsupported(format!(
                            "DROP TABLE: relation `{canonical}` is a {kind}, not a table"
                        )));
                    }
                    None if stmt.if_exists => {}
                    None => {
                        return Err(SQLError::Unsupported(format!(
                            "DROP TABLE: relation `{name}` does not exist"
                        )));
                    }
                }
            }
            engine
                .try_drop_tables(&tables, stmt.cascade)
                .map_err(|err| ddl_storage_error("DROP TABLE", err))?;
        }
        DropKind::Index => {
            let mut indexes = Vec::new();
            for name in &stmt.names {
                let Some(row) = engine
                    .catalog_index(name)
                    .map_err(|err| ddl_storage_error("DROP INDEX", err))?
                else {
                    if stmt.if_exists {
                        continue;
                    }
                    return Err(SQLError::Unsupported(format!(
                        "DROP INDEX: index `{name}` does not exist"
                    )));
                };
                indexes.push(row);
            }
            for row in indexes {
                drop_index_side_effects(engine, &row)?;
                engine
                    .try_drop_catalog_index(&row.name)
                    .map_err(|e| ddl_storage_error("DROP INDEX", e))?;
            }
        }
        DropKind::View => {
            let mut views = Vec::new();
            for name in &stmt.names {
                match engine
                    .try_resolve_relation_kind(name)
                    .map_err(|err| ddl_storage_error("DROP VIEW", err))?
                {
                    Some((canonical, "view")) => views.push(canonical),
                    Some((canonical, kind)) => {
                        return Err(SQLError::Unsupported(format!(
                            "DROP VIEW: relation `{canonical}` is a {kind}, not a view"
                        )));
                    }
                    None if stmt.if_exists => {}
                    None => {
                        return Err(SQLError::Unsupported(format!(
                            "DROP VIEW: relation `{name}` does not exist"
                        )));
                    }
                }
            }
            let drop_set = views
                .iter()
                .cloned()
                .collect::<std::collections::BTreeSet<_>>();
            for view in &views {
                let dependents = engine
                    .views_depending_on_relation(view)
                    .map_err(|err| ddl_storage_error("DROP VIEW", err))?
                    .into_iter()
                    .filter(|dependent| !drop_set.contains(dependent))
                    .collect::<Vec<_>>();
                if !dependents.is_empty() {
                    return Err(SQLError::Unsupported(format!(
                        "DROP VIEW `{view}` rejected: dependent view(s) `{}` still reference it",
                        dependents.join("`, `")
                    )));
                }
            }
            engine.drop_views(&views)?;
        }
        DropKind::Schema => {
            let mut schemas = Vec::new();
            for name in &stmt.names {
                match engine
                    .preflight_drop_schema(name)
                    .map_err(|err| ddl_storage_error("DROP SCHEMA", err))?
                {
                    true => schemas.push(name.clone()),
                    false if stmt.if_exists => {}
                    false => {
                        return Err(SQLError::Unsupported(format!(
                            "DROP SCHEMA: schema `{name}` does not exist"
                        )));
                    }
                }
            }
            for schema in schemas {
                engine
                    .drop_schema(&schema)
                    .map_err(|err| ddl_storage_error("DROP SCHEMA", err))?;
            }
        }
    }
    Ok(SQLResult::empty())
}

fn ddl_storage_error(action: &str, err: impl std::fmt::Display) -> SQLError {
    SQLError::Internal(format!("{action} failed in storage backend: {err}"))
}

fn drop_index_side_effects(engine: &Engine, row: &CatalogIndexRow) -> Result<(), SQLError> {
    if row.index_type.eq_ignore_ascii_case("gin") {
        drop_gin_index_side_effects(engine, row)?;
    } else if row.index_type.eq_ignore_ascii_case("ivf")
        || row.index_type.eq_ignore_ascii_case("hnsw")
    {
        drop_ivf_index_side_effects(engine, row)?;
    }
    Ok(())
}

fn catalog_index_columns(row: &CatalogIndexRow, action: &str) -> Result<Vec<String>, SQLError> {
    serde_json::from_str(&row.columns_json).map_err(|e| {
        SQLError::Internal(format!(
            "{action} `{}`: invalid index column metadata: {e}",
            row.name
        ))
    })
}

fn drop_gin_index_side_effects(engine: &Engine, row: &CatalogIndexRow) -> Result<(), SQLError> {
    let fields: std::collections::BTreeSet<String> = catalog_index_columns(row, "DROP INDEX")?
        .into_iter()
        .collect();
    let indexes = engine
        .list_catalog_indexes()
        .map_err(|err| ddl_storage_error("DROP INDEX", err))?;

    for field in fields {
        let mut still_referenced = false;
        for candidate in &indexes {
            if candidate.name == row.name
                || candidate.table_name != row.table_name
                || !candidate.index_type.eq_ignore_ascii_case("gin")
            {
                continue;
            }
            if catalog_index_columns(candidate, "DROP INDEX")?
                .iter()
                .any(|candidate_field| candidate_field == &field)
            {
                still_referenced = true;
                break;
            }
        }
        if !still_referenced {
            engine
                .drop_fts_field(&row.table_name, &field)
                .map_err(|err| {
                    SQLError::Internal(format!(
                        "DROP INDEX `{}`: failed to remove FTS field `{}`.`{field}`: {err}",
                        row.name, row.table_name
                    ))
                })?;
        }
    }
    Ok(())
}

fn drop_ivf_index_side_effects(engine: &Engine, row: &CatalogIndexRow) -> Result<(), SQLError> {
    let columns = catalog_index_columns(row, "DROP INDEX")?;
    for col in columns {
        match engine
            .column_type(&row.table_name, &col)
            .map_err(|err| ddl_storage_error("DROP INDEX", err))?
        {
            Some(ColumnType::Vector(dim) | ColumnType::Tensor(dim)) => {
                if !engine
                    .drop_ivf_vector_field_index(&row.table_name, col.clone(), dim)
                    .map_err(|err| ddl_storage_error("DROP INDEX vector field", err))?
                {
                    return Err(SQLError::Unsupported(format!(
                        "DROP INDEX `{}`: relation `{}` does not exist",
                        row.name, row.table_name
                    )));
                }
                engine
                    .drop_vector_index_metadata(&row.table_name, &col)
                    .map_err(|e| {
                        SQLError::Internal(format!(
                            "DROP INDEX `{}`: failed to drop IVF metadata for `{}`.`{col}`: {e}",
                            row.name, row.table_name
                        ))
                    })?;
            }
            Some(other) => {
                return Err(SQLError::Unsupported(format!(
                    "DROP INDEX `{}`: IVF column `{}`.`{col}` is no longer VECTOR or TENSOR, got {other:?}",
                    row.name, row.table_name
                )));
            }
            None => {
                return Err(SQLError::Unsupported(format!(
                    "DROP INDEX `{}`: column `{}`.`{col}` does not exist",
                    row.name, row.table_name
                )));
            }
        }
    }
    Ok(())
}

pub(super) fn run_alter_table(
    engine: &Engine,
    stmt: AlterTableStmt,
) -> Result<SQLResult, SQLError> {
    if matches!(
        &stmt.action,
        AlterTableAction::DropColumn { cascade: true, .. }
    ) {
        return Err(SQLError::Unsupported(
            "ALTER TABLE DROP COLUMN CASCADE is not supported; no schema or data was changed"
                .into(),
        ));
    }
    engine.with_implicit_transaction(move |engine| run_alter_table_inner(engine, stmt))
}

fn run_alter_table_inner(engine: &Engine, mut stmt: AlterTableStmt) -> Result<SQLResult, SQLError> {
    match engine
        .try_resolve_relation_kind(&stmt.table)
        .map_err(|err| ddl_storage_error("ALTER TABLE", err))?
    {
        Some((canonical, "table")) => stmt.table = canonical,
        Some((canonical, kind)) => {
            return Err(SQLError::Unsupported(format!(
                "ALTER TABLE: relation `{canonical}` is a {kind}, not a table"
            )));
        }
        None if stmt.if_exists => return Ok(SQLResult::empty()),
        None => {
            return Err(SQLError::Unsupported(format!(
                "ALTER TABLE: relation `{}` does not exist",
                stmt.table
            )));
        }
    }
    match stmt.action {
        AlterTableAction::AddColumn {
            column,
            if_not_exists,
        } => {
            let col_name = column.name.clone();
            if engine
                .try_table_has_column(&stmt.table, &col_name)
                .map_err(|err| ddl_storage_error("ALTER TABLE ADD COLUMN", err))?
            {
                if if_not_exists {
                    return Ok(SQLResult::empty());
                }
                return Err(SQLError::Unsupported(format!(
                    "ALTER TABLE ADD COLUMN: column `{col_name}` already exists"
                )));
            }
            match column.ty {
                ColumnType::Vector(dim) | ColumnType::Tensor(dim) => {
                    engine
                        .create_vector_field(&stmt.table, col_name.clone(), dim)
                        .map_err(|err| ddl_storage_error("ALTER TABLE vector field", err))?;
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
            let column_not_null = column.not_null;
            engine
                .try_register_column(&stmt.table, column)
                .map_err(|e| ddl_storage_error("ALTER TABLE ADD COLUMN", e))?;
            let default_expr = engine
                .try_column_default_expr(&stmt.table, &col_name)
                .map_err(|e| ddl_storage_error("ALTER TABLE ADD COLUMN default", e))?;
            backfill_added_column(
                engine,
                &stmt.table,
                &col_name,
                default_expr.as_ref(),
                column_not_null,
            )?;
            engine
                .try_persist_table_schema(&stmt.table)
                .map_err(|e| ddl_storage_error("ALTER TABLE ADD COLUMN", e))?;
        }
        AlterTableAction::DropColumn {
            name,
            if_exists,
            cascade: false,
        } => {
            if !engine
                .try_table_has_column(&stmt.table, &name)
                .map_err(|err| ddl_storage_error("ALTER TABLE DROP COLUMN", err))?
            {
                if if_exists {
                    return Ok(SQLResult::empty());
                }
                return Err(SQLError::Unsupported(format!(
                    "ALTER TABLE DROP COLUMN: column `{name}` does not exist"
                )));
            }
            engine
                .try_drop_column(&stmt.table, &name)
                .map_err(|e| ddl_storage_error("ALTER TABLE DROP COLUMN", e))?;
        }
        AlterTableAction::DropColumn { cascade: true, .. } => unreachable!(),
        AlterTableAction::RenameColumn { from, to } => {
            if !engine
                .try_table_has_column(&stmt.table, &from)
                .map_err(|err| ddl_storage_error("ALTER TABLE RENAME COLUMN", err))?
            {
                return Err(SQLError::Unsupported(format!(
                    "ALTER TABLE RENAME COLUMN: column `{from}` does not exist"
                )));
            }
            if engine
                .try_table_has_column(&stmt.table, &to)
                .map_err(|err| ddl_storage_error("ALTER TABLE RENAME COLUMN", err))?
            {
                return Err(SQLError::Unsupported(format!(
                    "ALTER TABLE RENAME COLUMN: column `{to}` already exists"
                )));
            }
            engine
                .try_rename_column(&stmt.table, &from, &to)
                .map_err(|e| ddl_storage_error("ALTER TABLE RENAME COLUMN", e))?;
        }
        AlterTableAction::RenameTable { to } => {
            if engine
                .try_has_table(&to)
                .map_err(|err| ddl_storage_error("ALTER TABLE RENAME", err))?
            {
                return Err(SQLError::Unsupported(format!(
                    "ALTER TABLE RENAME: relation `{to}` already exists"
                )));
            }
            if !engine
                .try_rename_table(&stmt.table, &to)
                .map_err(|e| ddl_storage_error("ALTER TABLE RENAME", e))?
            {
                return Err(SQLError::Unsupported(format!(
                    "ALTER TABLE RENAME: rename of `{}` failed",
                    stmt.table
                )));
            }
        }
        AlterTableAction::SetDefault { name, default } => {
            if !engine
                .set_column_default(&stmt.table, &name, Some(default))
                .map_err(|err| ddl_storage_error("ALTER COLUMN SET DEFAULT", err))?
            {
                return Err(SQLError::Unsupported(format!(
                    "ALTER TABLE ALTER COLUMN: column `{name}` does not exist"
                )));
            }
            engine
                .try_persist_table_schema(&stmt.table)
                .map_err(|e| ddl_storage_error("ALTER TABLE ALTER COLUMN", e))?;
        }
        AlterTableAction::DropDefault { name } => {
            if !engine
                .set_column_default(&stmt.table, &name, None)
                .map_err(|err| ddl_storage_error("ALTER COLUMN DROP DEFAULT", err))?
            {
                return Err(SQLError::Unsupported(format!(
                    "ALTER TABLE ALTER COLUMN: column `{name}` does not exist"
                )));
            }
            engine
                .try_persist_table_schema(&stmt.table)
                .map_err(|e| ddl_storage_error("ALTER TABLE ALTER COLUMN", e))?;
        }
        AlterTableAction::SetNotNull { name } => {
            ensure_column_exists(engine, &stmt.table, &name)?;
            ensure_existing_values_not_null(engine, &stmt.table, &name)?;
            engine
                .set_column_not_null(&stmt.table, &name, true)
                .map_err(|err| ddl_storage_error("ALTER COLUMN SET NOT NULL", err))?;
            engine
                .try_persist_table_schema(&stmt.table)
                .map_err(|e| ddl_storage_error("ALTER TABLE ALTER COLUMN", e))?;
        }
        AlterTableAction::DropNotNull { name } => {
            if !engine
                .set_column_not_null(&stmt.table, &name, false)
                .map_err(|err| ddl_storage_error("ALTER COLUMN DROP NOT NULL", err))?
            {
                return Err(SQLError::Unsupported(format!(
                    "ALTER TABLE ALTER COLUMN: column `{name}` does not exist"
                )));
            }
            engine
                .try_persist_table_schema(&stmt.table)
                .map_err(|e| ddl_storage_error("ALTER TABLE ALTER COLUMN", e))?;
        }
        AlterTableAction::AlterColumnType { name, ty } => {
            ensure_column_exists(engine, &stmt.table, &name)?;
            let old_ty = engine
                .column_type(&stmt.table, &name)
                .map_err(|err| ddl_storage_error("ALTER COLUMN TYPE", err))?;
            let old_was_vector =
                matches!(old_ty, Some(ColumnType::Vector(_) | ColumnType::Tensor(_)));
            let new_is_vector = matches!(&ty, ColumnType::Vector(_) | ColumnType::Tensor(_));

            // Row rewrites maintain every currently registered vector index.
            // Detach a vector/tensor index before converting its values to a
            // scalar type, otherwise the first converted scalar is fed back
            // into the old vector index. The enclosing ALTER transaction
            // restores both catalog and physical index state if conversion
            // of any row subsequently fails.
            if old_was_vector && !new_is_vector {
                engine
                    .try_drop_vector_indexes_for_column(&stmt.table, &name)
                    .map_err(|e| ddl_storage_error("ALTER TABLE ALTER COLUMN", e))?;
            }
            rewrite_column_values_to_type(engine, &stmt.table, &name, &ty)?;
            engine
                .set_column_type(&stmt.table, &name, &ty)
                .map_err(|err| ddl_storage_error("ALTER COLUMN TYPE", err))?;
            match ty {
                ColumnType::Text => {
                    if let Err(e) = engine.add_fts_field(&stmt.table, name.clone()) {
                        return Err(SQLError::Internal(format!("add_fts_field: {e}")));
                    }
                }
                ColumnType::Vector(dim) | ColumnType::Tensor(dim) => {
                    engine
                        .try_rebuild_vector_index_for_column(&stmt.table, &name, dim)
                        .map_err(|e| ddl_storage_error("ALTER TABLE ALTER COLUMN", e))?;
                }
                _ => {}
            }
            engine
                .try_persist_table_schema(&stmt.table)
                .map_err(|e| ddl_storage_error("ALTER TABLE ALTER COLUMN", e))?;
        }
    }
    Ok(SQLResult::empty())
}

fn ensure_column_exists(engine: &Engine, table: &str, column: &str) -> Result<(), SQLError> {
    if engine
        .try_table_has_column(table, column)
        .map_err(|err| ddl_storage_error("ALTER COLUMN", err))?
    {
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
    for doc_id in engine.table_doc_ids(table)? {
        let Some(doc) = engine.get_document(table, doc_id)? else {
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
    let cols = match engine
        .try_describe_table(table)
        .map_err(|err| ddl_storage_error("column type coercion", err))?
    {
        Some(c) => c,
        None => return Ok(value),
    };
    let Some(def) = cols.iter().find(|c| c.name == column) else {
        return Ok(value);
    };
    convert_value_to_column_type(value, &def.ty)
}

fn coerce_json_value(value: Value) -> Result<Value, SQLError> {
    match value {
        Value::Str(s) => serde_json::from_str::<serde_json::Value>(&s)
            .map(json_to_core_value)
            .map_err(|error| {
                SQLError::TypeMismatch(format!("cannot cast string to JSON: {error}"))
            }),
        other => Ok(other),
    }
}

fn float_to_integer(value: f64) -> Result<i64, SQLError> {
    // `i64::MAX as f64` rounds to 2^63, which itself is outside the range.
    const I64_UPPER_EXCLUSIVE: f64 = 9_223_372_036_854_775_808.0;
    const I64_LOWER_INCLUSIVE: f64 = -9_223_372_036_854_775_808.0;
    if !value.is_finite() || !(I64_LOWER_INCLUSIVE..I64_UPPER_EXCLUSIVE).contains(&value) {
        return Err(SQLError::TypeMismatch(format!(
            "cannot cast {value:?} to integer: value is outside BIGINT range"
        )));
    }
    Ok(value as i64)
}

fn rewrite_column_values_to_type(
    engine: &Engine,
    table: &str,
    column: &str,
    ty: &ColumnType,
) -> Result<(), SQLError> {
    for doc_id in engine.table_doc_ids(table)? {
        let Some(doc) = engine.get_document(table, doc_id)? else {
            continue;
        };
        let Some(value) = doc.get(column).cloned() else {
            continue;
        };
        let converted = convert_value_to_column_type(value, ty)?;
        let mut updates: BTreeMap<String, Value> = BTreeMap::new();
        updates.insert(column.to_string(), converted.clone());
        let mut vectors: RowUpdateVectors = BTreeMap::new();
        if matches!(ty, ColumnType::Vector(_) | ColumnType::Tensor(_)) {
            vectors.insert(column.to_string(), index_vectors_for_type(&converted, ty)?);
        }
        engine.update_document_fields_with_vector_values(table, doc_id, updates, vectors)?;
    }
    Ok(())
}

pub(crate) fn convert_value_to_column_type(
    value: Value,
    ty: &ColumnType,
) -> Result<Value, SQLError> {
    if matches!(value, Value::Null) {
        return Ok(Value::Null);
    }
    match ty {
        ColumnType::Integer => match value {
            Value::Int(_) => Ok(value),
            Value::Float(f) => float_to_integer(f).map(Value::Int),
            Value::Decimal(d) => d
                .to_i64_trunc()
                .map(Value::Int)
                .ok_or_else(|| SQLError::TypeMismatch("cannot cast decimal to integer".into())),
            Value::Bool(b) => Ok(Value::Int(i64::from(b))),
            Value::Str(s) => s
                .parse::<i64>()
                .map(Value::Int)
                .map_err(|e| SQLError::TypeMismatch(format!("cannot cast `{s}` to integer: {e}"))),
            other => Err(SQLError::TypeMismatch(format!(
                "cannot cast {other:?} to integer"
            ))),
        },
        ColumnType::Boolean => match value {
            Value::Bool(_) => Ok(value),
            Value::Str(text) => parse_boolean_text(&text)
                .map(Value::Bool)
                .ok_or_else(|| SQLError::TypeMismatch(format!("cannot cast `{text}` to boolean"))),
            other => Err(SQLError::TypeMismatch(format!(
                "cannot cast {other:?} to boolean"
            ))),
        },
        ColumnType::Text => Ok(Value::Str(value_to_text(&value))),
        ColumnType::Real => match value {
            Value::Float(_) => Ok(value),
            Value::Int(i) => Ok(Value::Float(i as f64)),
            Value::Decimal(d) => d
                .to_f64()
                .map(Value::Float)
                .ok_or_else(|| SQLError::TypeMismatch("cannot cast decimal to real".into())),
            Value::Bool(b) => Ok(Value::Float(if b { 1.0 } else { 0.0 })),
            Value::Str(s) => s
                .parse::<f64>()
                .map(Value::Float)
                .map_err(|e| SQLError::TypeMismatch(format!("cannot cast `{s}` to real: {e}"))),
            other => Err(SQLError::TypeMismatch(format!(
                "cannot cast {other:?} to real"
            ))),
        },
        ColumnType::Numeric { precision, scale } => {
            let decimal = match value {
                Value::Decimal(d) => d,
                Value::Int(i) => DecimalValue::from_i64(i),
                Value::Float(f) => DecimalValue::from_f64_lossy(f).ok_or_else(|| {
                    SQLError::TypeMismatch(format!("cannot cast {f:?} to numeric"))
                })?,
                Value::Bool(b) => DecimalValue::from_bool(b),
                Value::Str(s) => DecimalValue::parse(&s).ok_or_else(|| {
                    SQLError::TypeMismatch(format!("cannot cast `{s}` to numeric"))
                })?,
                other => {
                    return Err(SQLError::TypeMismatch(format!(
                        "cannot cast {other:?} to numeric"
                    )));
                }
            };
            let rounded = match scale {
                Some(s) => decimal.round_to_scale(*s).ok_or_else(|| {
                    SQLError::TypeMismatch(format!("cannot round numeric to scale {s}"))
                })?,
                None => decimal,
            };
            if let Some(precision) = precision {
                let scale = scale.unwrap_or(0);
                if !rounded.fits_precision(*precision, scale) {
                    return Err(SQLError::TypeMismatch(format!(
                        "numeric field overflow: value {} exceeds precision {precision}, scale {scale}",
                        rounded.to_sql_string()
                    )));
                }
            }
            Ok(Value::Decimal(rounded))
        }
        ColumnType::Json | ColumnType::JsonB => coerce_json_value(value),
        ColumnType::Bytea => Ok(match value {
            Value::Bytes(_) => value,
            Value::Str(s) => Value::Bytes(s.into_bytes()),
            other => Value::Bytes(value_to_text(&other).into_bytes()),
        }),
        ColumnType::Array(element_type) => {
            let items = match value {
                Value::List(items) => items,
                Value::Str(text) => uqa_sql::expr::parse_pg_array_literal(&text)?,
                other => {
                    return Err(SQLError::TypeMismatch(format!(
                        "cannot cast {other:?} to {}[]",
                        column_type_name(element_type)
                    )))
                }
            };
            let converted = items
                .into_iter()
                .map(|item| convert_value_to_column_type(item, element_type))
                .collect::<Result<Vec<_>, _>>()?;
            uqa_sql::expr::array_dimensions(&converted)?;
            Ok(Value::List(converted))
        }
        ColumnType::Date
        | ColumnType::Time
        | ColumnType::TimeTz
        | ColumnType::Timestamp
        | ColumnType::TimestampTz => convert_temporal_value(value, ty),
        ColumnType::Vector(dim) => {
            let vector = value_to_vector(&value)?;
            validate_vector_dimensions(*dim, vector.len())?;
            Ok(vector_to_value(vector))
        }
        ColumnType::Tensor(dim) => {
            let tensor = value_to_tensor(&value)?;
            for vector in &tensor {
                validate_vector_dimensions(*dim, vector.len())?;
            }
            Ok(Value::List(
                tensor.into_iter().map(vector_to_value).collect(),
            ))
        }
    }
}

fn vector_to_value(vector: Vec<f32>) -> Value {
    Value::List(
        vector
            .into_iter()
            .map(|value| Value::Float(f64::from(value)))
            .collect(),
    )
}

pub(crate) fn validate_vector_dimensions(expected: u32, actual: usize) -> Result<(), SQLError> {
    let expected = usize::try_from(expected).map_err(|_| {
        SQLError::TypeMismatch(format!(
            "declared vector dimension {expected} exceeds the platform usize range"
        ))
    })?;
    if actual == expected {
        Ok(())
    } else {
        Err(SQLError::VectorDimMismatch { expected, actual })
    }
}

pub(super) fn column_type_name(ty: &ColumnType) -> &'static str {
    match ty {
        ColumnType::Integer => "integer",
        ColumnType::Boolean => "boolean",
        ColumnType::Text => "text",
        ColumnType::Real => "real",
        ColumnType::Numeric { .. } => "numeric",
        ColumnType::Json => "json",
        ColumnType::JsonB => "jsonb",
        ColumnType::Bytea => "bytea",
        ColumnType::Array(_) => "array",
        ColumnType::Date => "date",
        ColumnType::Time => "time",
        ColumnType::TimeTz => "time with time zone",
        ColumnType::Timestamp => "timestamp",
        ColumnType::TimestampTz => "timestamp with time zone",
        ColumnType::Vector(_) => "vector",
        ColumnType::Tensor(_) => "tensor",
    }
}

fn parse_boolean_text(text: &str) -> Option<bool> {
    match text.trim().to_ascii_lowercase().as_str() {
        "true" | "t" | "yes" | "y" | "on" | "1" => Some(true),
        "false" | "f" | "no" | "n" | "off" | "0" => Some(false),
        _ => None,
    }
}

fn convert_temporal_value(value: Value, ty: &ColumnType) -> Result<Value, SQLError> {
    match value {
        Value::Temporal(temporal) => coerce_temporal_kind(temporal, ty)
            .map(Value::Temporal)
            .ok_or_else(|| {
                SQLError::TypeMismatch(format!(
                    "cannot cast temporal value to {}",
                    column_type_name(ty)
                ))
            }),
        other => {
            let text = value_to_text(&other);
            let parsed = parse_temporal_text_for_type(&text, ty);
            parsed.map(Value::Temporal).ok_or_else(|| {
                SQLError::TypeMismatch(format!("cannot cast `{text}` to {}", column_type_name(ty)))
            })
        }
    }
}

fn coerce_temporal_kind(value: TemporalValue, ty: &ColumnType) -> Option<TemporalValue> {
    match (ty, value) {
        (ColumnType::Date, value @ TemporalValue::Date { .. })
        | (ColumnType::Time, value @ TemporalValue::Time { .. })
        | (ColumnType::TimeTz, value @ TemporalValue::TimeTz { .. })
        | (ColumnType::Timestamp, value @ TemporalValue::Timestamp { .. })
        | (ColumnType::TimestampTz, value @ TemporalValue::TimestampTz { .. }) => Some(value),
        (ColumnType::Timestamp, TemporalValue::TimestampTz { micros }) => {
            Some(TemporalValue::Timestamp { micros })
        }
        (ColumnType::TimestampTz, TemporalValue::Timestamp { micros }) => {
            Some(TemporalValue::TimestampTz { micros })
        }
        _ => None,
    }
}

fn parse_temporal_text_for_type(text: &str, ty: &ColumnType) -> Option<TemporalValue> {
    match ty {
        ColumnType::Date => TemporalValue::parse_date(text),
        ColumnType::Time => TemporalValue::parse_time(text),
        ColumnType::TimeTz => TemporalValue::parse_time_tz(text),
        ColumnType::Timestamp => TemporalValue::parse_timestamp(text).or_else(|| {
            TemporalValue::parse_timestamp_tz(text).and_then(|value| match value {
                TemporalValue::TimestampTz { micros } => Some(TemporalValue::Timestamp { micros }),
                _ => None,
            })
        }),
        ColumnType::TimestampTz => TemporalValue::parse_timestamp_tz(text),
        _ => None,
    }
}

pub(super) fn value_to_text(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::Bool(b) => b.to_string(),
        Value::Int(i) => i.to_string(),
        Value::Float(f) => f.to_string(),
        Value::Decimal(d) => d.to_sql_string(),
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
            } else if let Some(d) = DecimalValue::parse(&n.to_string()) {
                Value::Decimal(d)
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
        Value::Float(f) => serde_json::Number::from_f64(*f).map_or_else(
            || {
                let label = if f.is_nan() {
                    "NaN"
                } else if f.is_sign_positive() {
                    "Infinity"
                } else {
                    "-Infinity"
                };
                serde_json::Value::String(label.to_string())
            },
            serde_json::Value::Number,
        ),
        Value::Decimal(d) => d
            .to_f64()
            .and_then(serde_json::Number::from_f64)
            .map_or_else(
                || serde_json::Value::String(d.to_sql_string()),
                serde_json::Value::Number,
            ),
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
        serde_json::Value::Array(_) | serde_json::Value::Object(_) => Value::Str(value.to_string()),
    }
}

pub(super) fn json_table_arg(value: &Value, name: &str) -> Result<serde_json::Value, SQLError> {
    match value {
        Value::Str(s) => serde_json::from_str::<serde_json::Value>(s)
            .map_err(|e| SQLError::TypeMismatch(format!("{name}: invalid JSON: {e}"))),
        other => Ok(core_value_to_json(other)),
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
    let doc_ids = engine.table_doc_ids(table)?;
    if doc_ids.is_empty() {
        return Ok(());
    }
    let default_value = if let Some(expr) = default_expr {
        eval_lowered_expression(engine, expr, None, &[])?
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
    let vector_value: Option<Vec<Vec<f32>>> = match engine
        .column_type(table, column)
        .map_err(|err| ddl_storage_error("ALTER TABLE ADD COLUMN", err))?
    {
        Some(ty) if matches!(ty, ColumnType::Vector(_) | ColumnType::Tensor(_)) => {
            Some(index_vectors_for_type(&default_value, &ty)?)
        }
        Some(_) | None => None,
    };
    for doc_id in doc_ids {
        let mut updates: BTreeMap<String, Value> = BTreeMap::new();
        updates.insert(column.to_string(), default_value.clone());
        let mut vectors: RowUpdateVectors = BTreeMap::new();
        if let Some(v) = vector_value.as_ref() {
            vectors.insert(column.to_string(), v.clone());
        }
        engine.update_document_fields_with_vector_values(table, doc_id, updates, vectors)?;
    }
    Ok(())
}

// DDL
// -------------------------------------------------------------------------

pub(super) fn run_create_table(engine: &Engine, c: CreateTable) -> Result<SQLResult, SQLError> {
    engine.transaction(move |engine| run_create_table_inner(engine, c))
}

fn run_create_table_inner(engine: &Engine, c: CreateTable) -> Result<SQLResult, SQLError> {
    if engine
        .try_has_table(&c.name)
        .map_err(|err| ddl_storage_error("CREATE TABLE", err))?
    {
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
    engine
        .create_default_table(c.name.clone(), Vec::new())
        .map_err(|err| ddl_storage_error("CREATE TABLE", err))?;
    for (field, dim) in vector_fields {
        engine
            .create_vector_field(&c.name, field, dim)
            .map_err(|err| ddl_storage_error("CREATE TABLE vector field", err))?;
    }
    for col in &c.columns {
        engine
            .try_register_column(&c.name, col.clone())
            .map_err(|e| ddl_storage_error("CREATE TABLE column", e))?;
    }
    engine
        .register_table_constraints(
            &c.name,
            c.checks.clone(),
            c.foreign_keys.clone(),
            c.key_constraints.clone(),
        )
        .map_err(|err| ddl_storage_error("CREATE TABLE constraints", err))?;
    engine
        .try_persist_table_schema(&c.name)
        .map_err(|e| ddl_storage_error("CREATE TABLE", e))?;
    engine
        .refresh_value_indexes_for_table(&c.name)
        .map_err(|e| ddl_storage_error("CREATE TABLE btree indexes", e))?;
    Ok(SQLResult::empty())
}

pub(super) fn run_create_index(engine: &Engine, c: CreateIndex) -> Result<SQLResult, SQLError> {
    // Every accepted access method has a matching physical implementation.
    // Reject unknown methods before allocating a name or touching any table,
    // index, analyzer, or catalog state.
    let am = c.access_method.to_ascii_lowercase();
    if !matches!(am.as_str(), "" | "btree" | "gin" | "ivf" | "hnsw") {
        return Err(SQLError::Unsupported(format!(
            "CREATE INDEX access method `{}` is not supported",
            c.access_method
        )));
    }

    let name = if let Some(name) = c.name.as_ref() {
        if engine
            .has_catalog_index(name)
            .map_err(|err| ddl_storage_error("CREATE INDEX", err))?
        {
            if c.if_not_exists {
                return Ok(SQLResult::empty());
            }
            return Err(SQLError::Unsupported(format!(
                "Index `{name}` already exists"
            )));
        }
        name.clone()
    } else {
        allocate_default_index_name(engine, &c.table, &c.columns)?
    };

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
        "" | "btree" => {}
        "ivf" | "hnsw" => {
            let params = parse_ivf_index_params(&c.options)?;
            for col in &c.columns {
                match engine
                    .column_type(&c.table, col)
                    .map_err(|err| ddl_storage_error("CREATE INDEX", err))?
                {
                    Some(ColumnType::Vector(dim) | ColumnType::Tensor(dim)) => {
                        if !engine
                            .rebuild_ivf_vector_field(&c.table, col.clone(), dim, params)
                            .map_err(|err| ddl_storage_error("CREATE INDEX vector field", err))?
                        {
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
        _ => unreachable!("access method was validated above"),
    }
    // Persist the CREATE INDEX statement itself so reopen sees the
    // same set of registered indexes. The engine layer parses
    // `parameters_json` back into `(key, value)` pairs and re-runs
    // any access-method-specific side effects (e.g. add_fts_field
    // for `gin`) on restore.
    let catalog_index_type = match am.as_str() {
        "" => "btree",
        "hnsw" => "ivf",
        other => other,
    };
    engine
        .try_register_catalog_index(&name, catalog_index_type, &c.table, &c.columns, &c.options)
        .map_err(|e| ddl_storage_error("CREATE INDEX", e))?;
    Ok(SQLResult::empty())
}

fn allocate_default_index_name(
    engine: &Engine,
    table: &str,
    columns: &[String],
) -> Result<String, SQLError> {
    fn component(raw: &str) -> String {
        let mut out = String::with_capacity(raw.len());
        let mut previous_was_separator = false;
        for ch in raw.chars() {
            if ch.is_alphanumeric() || ch == '_' {
                out.extend(ch.to_lowercase());
                previous_was_separator = false;
            } else if !previous_was_separator && !out.is_empty() {
                out.push('_');
                previous_was_separator = true;
            }
        }
        while out.ends_with('_') {
            out.pop();
        }
        out
    }

    let mut parts = table
        .split('.')
        .map(component)
        .chain(columns.iter().map(|column| component(column)))
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    if parts.is_empty() {
        parts.push("index".to_string());
    }
    let base = format!("{}_idx", parts.join("_"));
    let existing = engine
        .list_catalog_indexes()
        .map_err(|err| ddl_storage_error("CREATE INDEX", err))?
        .into_iter()
        .map(|row| row.name)
        .collect::<std::collections::BTreeSet<_>>();
    if !existing.contains(&base) {
        return Ok(base);
    }
    for suffix in 1_u64.. {
        let candidate = format!("{base}_{suffix}");
        if !existing.contains(&candidate) {
            return Ok(candidate);
        }
    }
    unreachable!("u64 index-name suffix space is non-empty")
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

#[cfg(test)]
mod tests {
    use super::{coerce_json_value, float_to_integer};
    use uqa_core::Value;

    #[test]
    fn integer_coercion_rejects_non_finite_and_out_of_range_floats() {
        assert_eq!(float_to_integer(12.9).unwrap(), 12);
        for value in [
            f64::NAN,
            f64::INFINITY,
            f64::NEG_INFINITY,
            9_223_372_036_854_775_808.0,
            -9_223_372_036_854_777_856.0,
        ] {
            assert!(float_to_integer(value).is_err(), "accepted {value:?}");
        }
    }

    #[test]
    fn json_coercion_rejects_invalid_json_strings() {
        assert!(coerce_json_value(Value::Str("{invalid".into())).is_err());
        assert!(matches!(
            coerce_json_value(Value::Str("{\"ok\":true}".into())).unwrap(),
            Value::Map(_)
        ));
    }
}
