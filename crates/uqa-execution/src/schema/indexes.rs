//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Validate SQL key ordering and uniqueness before publishing a B-tree index.
use crate::mutation::constraints::{
    context::MutationRead,
    index_keys::{index_key_values, index_predicate_accepts, IndexExpressionContext},
};
use crate::query::runtime::QueryMemorySettings;
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
pub fn validate_index_keys(
    context: &IndexBuildContext<'_>,
    statement: &CreateIndex,
    name: &str,
    key_types: &[uqa_sql::ast::ColumnType],
) -> Result<(), SQLError> {
    if statement.unique {
        validate_unique_index_method(statement)?;
    } else if !key_types
        .iter()
        .any(uqa_sql::expr::type_comparison_can_fail)
    {
        return Ok(());
    }
    let method = uqa_sql::schema::indexes::options::index_access_method(statement)?;
    if !method.is_empty() && method != "btree" {
        return Ok(());
    }
    let hierarchy = context.catalog.table_hierarchy(&statement.table)?;
    if statement.unique {
        validate_unique_partition_columns(statement, &hierarchy)?;
    }
    let tables = if hierarchy.partition_spec.is_some() {
        context.catalog.scan_tables(&statement.table)?
    } else {
        vec![statement.table.clone()]
    };
    let mut keys =
        build_keys::IndexBuildKeys::new(statement.columns.len(), context.memory.work_mem_bytes()?);
    for table in tables {
        for id in context.reads.live_table_doc_ids(&table)? {
            let document = context
                .reads
                .get_document(&table, id)?
                .ok_or_else(|| SQLError::Internal("index build lost a visible row".into()))?;
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
            keys.push(values)?;
        }
    }
    keys.validate(name, statement.unique, statement.nulls_not_distinct)
}

mod binding;
mod build_keys;
pub mod constraint_names;
pub mod creation;
pub mod renaming;

pub mod registration;
pub mod registry;
pub mod removal;
pub mod restoration;
pub mod routines;
