//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Validate visible rows before publishing a unique index.
use crate::mutation::constraints::{
    context::MutationRead,
    index_keys::{index_key_values, index_predicate_accepts, IndexExpressionContext},
};
use crate::query::runtime::QueryMemorySettings;
use crate::{physical::physical_exec_error, ExactRowSet};
use uqa_core::Value;
use uqa_sql::schema::indexes::unique::{
    validate_unique_index_method, validate_unique_partition_columns,
};
use uqa_sql::{
    ast::{CreateIndex, TableHierarchy},
    SQLError,
};

pub trait IndexBuildCatalog {
    fn table_hierarchy(&self, table: &str) -> Result<TableHierarchy, SQLError>;
    fn scan_tables(&self, table: &str) -> Result<Vec<String>, SQLError>;
}
pub struct IndexBuildContext<'a> {
    pub catalog: &'a dyn IndexBuildCatalog,
    pub reads: &'a dyn MutationRead,
    pub expressions: IndexExpressionContext<'a>,
    pub memory: &'a dyn QueryMemorySettings,
}
pub fn validate_unique_index(
    context: &IndexBuildContext<'_>,
    statement: &CreateIndex,
    name: &str,
) -> Result<(), SQLError> {
    if !statement.unique {
        return Ok(());
    }
    validate_unique_index_method(statement)?;
    let hierarchy = context.catalog.table_hierarchy(&statement.table)?;
    validate_unique_partition_columns(statement, &hierarchy)?;
    let tables = if hierarchy.partition_spec.is_some() {
        context.catalog.scan_tables(&statement.table)?
    } else {
        vec![statement.table.clone()]
    };
    let mut keys = ExactRowSet::new(context.memory.work_mem_bytes()?);
    for table in tables {
        for id in context.reads.live_table_doc_ids(&table)? {
            let document = context.reads.get_document(&table, id)?.ok_or_else(|| {
                SQLError::Internal("unique-index build lost a visible row".into())
            })?;
            if !index_predicate_accepts(
                context.expressions,
                &table,
                statement.predicate.as_deref(),
                &document,
            )? {
                continue;
            }
            let values =
                index_key_values(context.expressions, &table, &statement.columns, &document)?;
            if !statement.nulls_not_distinct
                && values.iter().any(|value| matches!(value, Value::Null))
            {
                continue;
            }
            if !keys.insert_values(&values).map_err(physical_exec_error)? {
                return Err(SQLError::Routine {
                    sqlstate: "23505".into(),
                    message: format!(
                        r#"could not create unique index "{name}": key is duplicated"#
                    ),
                });
            }
        }
    }
    Ok(())
}

pub mod creation;
