//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Sequence DDL and CREATE TABLE AS execution.

use std::collections::BTreeSet;

use super::{ddl_storage_error, ColumnType, Document, Engine, SQLError, SQLParam, SQLResult};

const POSTGRES_SYSTEM_COLUMNS: [&str; 6] = ["tableoid", "xmin", "cmin", "xmax", "cmax", "ctid"];

pub(in crate::sql) struct CreateTableAsExecution<'a> {
    pub name: &'a str,
    pub if_not_exists: bool,
    pub column_names: &'a [String],
    pub with_no_data: bool,
    pub persistence: uqa_sql::ast::RelationPersistence,
    pub on_commit: uqa_sql::ast::OnCommitAction,
    pub query: &'a uqa_planner::QueryPlan,
    pub params: &'a [SQLParam],
}

pub(in crate::sql) fn run_create_sequence(
    engine: &Engine,
    s: uqa_sql::ast::CreateSequence,
) -> Result<SQLResult, SQLError> {
    engine
        .create_sequence_with_persistence(
            &s.name,
            s.start,
            s.increment,
            s.if_not_exists,
            s.persistence,
        )
        .map_err(SQLError::Unsupported)?;
    Ok(SQLResult::empty())
}

pub(in crate::sql) fn run_alter_sequence(
    engine: &Engine,
    s: uqa_sql::ast::AlterSequence,
) -> Result<SQLResult, SQLError> {
    engine
        .alter_sequence_if_exists(&s.name, s.restart, s.increment, s.start, s.if_exists)
        .map_err(SQLError::Unsupported)?;
    Ok(SQLResult::empty())
}

pub(in crate::sql) fn run_create_table_as(
    engine: &Engine,
    execution: CreateTableAsExecution<'_>,
) -> Result<SQLResult, SQLError> {
    if engine.transaction_depth() != 0 {
        run_create_table_as_inner(engine, &execution)
    } else {
        engine.transaction(move |engine| run_create_table_as_inner(engine, &execution))
    }
}

fn run_create_table_as_inner(
    engine: &Engine,
    execution: &CreateTableAsExecution<'_>,
) -> Result<SQLResult, SQLError> {
    let name = if execution.persistence == uqa_sql::ast::RelationPersistence::Temporary {
        engine
            .try_temporary_relation_name_for_create(execution.name)
            .map_err(SQLError::Unsupported)?
    } else {
        engine
            .try_relation_name_for_create(execution.name)
            .map_err(SQLError::Unsupported)?
    };
    if engine
        .relation_kind_at(&name)
        .map_err(|err| ddl_storage_error("CREATE TABLE AS", err))?
        .is_some()
    {
        if execution.if_not_exists {
            return Ok(SQLResult::empty());
        }
        return Err(SQLError::Routine {
            sqlstate: "42P07".into(),
            message: format!("relation \"{name}\" already exists"),
        });
    }
    let ctes = crate::sql::select::CteScope::new();
    let query_schema = crate::sql::select::analyze_query_plan_schema(
        engine,
        execution.query,
        execution.params,
        &ctes,
        None,
    )?;
    let columns = create_table_as_columns(&query_schema, execution.column_names)?;
    let result = if execution.with_no_data {
        None
    } else {
        let result =
            crate::sql::select::execute_query_plan(engine, execution.query, execution.params)?;
        if result.columns.len() != columns.len() {
            return Err(SQLError::Internal(format!(
                "CREATE TABLE AS query schema width {} changed to {} during execution",
                columns.len(),
                result.columns.len()
            )));
        }
        Some(result)
    };
    create_table_as_relation(
        engine,
        &name,
        &columns,
        execution.persistence,
        execution.on_commit,
    )?;
    let affected = result.as_ref().map_or(Ok(0), |result| {
        materialize_create_table_as_rows(engine, &name, &columns, result)
    })?;
    Ok(SQLResult::from_affected(affected))
}

fn create_table_as_relation(
    engine: &Engine,
    name: &str,
    columns: &[uqa_sql::ast::ColumnDef],
    persistence: uqa_sql::ast::RelationPersistence,
    on_commit: uqa_sql::ast::OnCommitAction,
) -> Result<(), SQLError> {
    let analyzer = uqa_analysis::analyzer::standard_analyzer("english");
    engine
        .create_table_with_lifecycle(name, analyzer, Vec::new(), persistence, on_commit)
        .map_err(|err| ddl_storage_error("CREATE TABLE AS", err))?;
    for column in columns {
        if let ColumnType::Vector(dimensions) | ColumnType::Tensor(dimensions) = column.ty {
            engine
                .create_vector_field(name, column.name.clone(), dimensions)
                .map_err(|err| ddl_storage_error("CREATE TABLE AS vector field", err))?;
        }
    }
    let table = engine
        .try_table(name)
        .map_err(|err| ddl_storage_error("CREATE TABLE AS schema", err))?
        .ok_or_else(|| {
            SQLError::Internal(format!("new CREATE TABLE AS relation `{name}` disappeared"))
        })?;
    *table.columns.write() = columns.to_vec();
    engine
        .try_persist_table_schema(name)
        .map_err(|err| ddl_storage_error("CREATE TABLE AS schema", err))?;
    Ok(())
}

fn materialize_create_table_as_rows(
    engine: &Engine,
    name: &str,
    columns: &[uqa_sql::ast::ColumnDef],
    result: &SQLResult,
) -> Result<u64, SQLError> {
    for (row_index, _) in result.rows.iter().enumerate() {
        let doc_id = u64::try_from(row_index)
            .ok()
            .and_then(|index| index.checked_add(1))
            .ok_or_else(|| SQLError::Internal("CREATE TABLE AS row count overflow".into()))?;
        let mut document = Document::new();
        for (column_index, column) in columns.iter().enumerate() {
            let value = result
                .value_at(row_index, column_index)
                .cloned()
                .ok_or_else(|| {
                    SQLError::Internal(format!(
                        "CREATE TABLE AS row {row_index} is missing column {column_index}"
                    ))
                })?;
            document.insert(column.name.clone(), value);
        }
        let vectors = crate::sql::dml::document_vectors(engine, name, &document)?;
        engine.add_document_with_vector_values(name, doc_id, document, vectors)?;
    }
    u64::try_from(result.rows.len())
        .map_err(|_| SQLError::Internal("CREATE TABLE AS row count overflow".into()))
}

fn create_table_as_columns(
    query_schema: &uqa_execution::RowSchema,
    column_names: &[String],
) -> Result<Vec<uqa_sql::ast::ColumnDef>, SQLError> {
    if column_names.len() > query_schema.len() {
        return Err(SQLError::Routine {
            sqlstate: "42601".into(),
            message: "too many column names were specified".into(),
        });
    }
    let names = query_schema
        .columns()
        .iter()
        .enumerate()
        .map(|(position, name)| {
            column_names
                .get(position)
                .cloned()
                .unwrap_or_else(|| name.clone())
        })
        .collect::<Vec<_>>();
    let mut seen = BTreeSet::new();
    for name in &names {
        if POSTGRES_SYSTEM_COLUMNS.contains(&name.as_str()) {
            return Err(SQLError::Routine {
                sqlstate: "42701".into(),
                message: format!("column name \"{name}\" conflicts with a system column name"),
            });
        }
        if !seen.insert(name) {
            return Err(SQLError::Routine {
                sqlstate: "42701".into(),
                message: format!("column \"{name}\" specified more than once"),
            });
        }
    }
    Ok(names
        .into_iter()
        .enumerate()
        .map(|(position, name)| uqa_sql::ast::ColumnDef {
            name,
            ty: query_schema
                .column_type(position)
                .cloned()
                .unwrap_or(ColumnType::Text),
            primary_key: false,
            not_null: false,
            not_null_explicit: false,
            not_null_name: None,
            auto_increment: false,
            unique: false,
            default: None,
            generated: None,
            check: None,
            check_name: None,
            check_enforced: true,
            references: None,
        })
        .collect())
}
