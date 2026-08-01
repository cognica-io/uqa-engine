//! CREATE TABLE execution.

use super::{ddl_storage_error, ColumnType, CreateTable, Engine, SQLError, SQLResult};

// -------------------------------------------------------------------------

pub(in crate::sql) fn run_create_table(
    engine: &Engine,
    c: CreateTable,
) -> Result<SQLResult, SQLError> {
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
