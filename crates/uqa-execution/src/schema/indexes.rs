//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Validate SQL key ordering and uniqueness before publishing a B-tree index.
pub mod diskann;
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
    /// The partitions below `table`, each after its parent with siblings in partition bound order.
    fn partition_tree(
        &self,
        table: &str,
    ) -> Result<Vec<uqa_sql::semantics::partition::PartitionTreeNode>, SQLError>;
}
pub struct IndexBuildContext<'a> {
    pub catalog: &'a dyn IndexBuildCatalog,
    pub reads: &'a dyn MutationRead,
    pub expressions: IndexExpressionContext<'a>,
    pub memory: &'a dyn QueryMemorySettings,
    pub description: &'a dyn unique_build::IndexKeyDescription,
}
pub fn validate_index_keys(
    context: &IndexBuildContext<'_>,
    statement: &CreateIndex,
    name: &str,
    key_types: &[uqa_sql::ast::ColumnType],
) -> Result<(), SQLError> {
    validate_index_declaration(context.catalog, statement)?;
    validate_index_rows(context, statement, name, key_types)
}

/// Declaration errors precede name collisions; existing row validation belongs to a real build.
pub fn validate_index_declaration(
    catalog: &dyn IndexBuildCatalog,
    statement: &CreateIndex,
) -> Result<(), SQLError> {
    if statement.unique {
        validate_unique_index_method(statement)?;
        validate_unique_partition_columns(statement, &catalog.table_hierarchy(&statement.table)?)?;
    }
    Ok(())
}

/// Whether building the index compares its keys: a unique build looks for a repeated key, and any build fails on a key whose comparison fails.
fn build_compares_keys(
    statement: &CreateIndex,
    key_types: &[uqa_sql::ast::ColumnType],
) -> Result<bool, SQLError> {
    if !statement.unique
        && !key_types
            .iter()
            .any(uqa_sql::expr::type_comparison_can_fail)
    {
        return Ok(false);
    }
    let method = uqa_sql::schema::indexes::options::index_access_method(statement)?;
    Ok(method.is_empty() || method == "btree")
}

/// Check the rows of the index's table, and of every partition below it, as one build under the index's name.
pub(super) fn validate_index_rows(
    context: &IndexBuildContext<'_>,
    statement: &CreateIndex,
    name: &str,
    key_types: &[uqa_sql::ast::ColumnType],
) -> Result<(), SQLError> {
    if !build_compares_keys(statement, key_types)? {
        return Ok(());
    }
    let hierarchy = context.catalog.table_hierarchy(&statement.table)?;
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
    let Some(values) = keys.first_duplicate(
        statement.unique,
        statement.nulls_not_distinct,
        context.expressions.values,
    )?
    else {
        return Ok(());
    };
    Err(unique_build::duplicated_index_key(
        name,
        context.description.describe_index_key(
            &statement.table,
            &statement.columns,
            key_types,
            &values,
        )?,
    ))
}

/// Check the rows that a new unique index of a partitioned table is built from, as `PostgreSQL`'s `DefineIndex` recurses: each partition that builds an index of its own is visited after its parent, a partitioned one is checked against its own partition key and a leaf's rows are built into its index, named in `planned` by partition. A partition that adopts an index it already has is absent from `planned` and is skipped with the partitions below it.
pub(super) fn validate_partition_index_rows(
    context: &IndexBuildContext<'_>,
    statement: &CreateIndex,
    key_types: &[uqa_sql::ast::ColumnType],
    planned: &std::collections::BTreeMap<String, String>,
) -> Result<(), SQLError> {
    let mut adopted = std::collections::BTreeSet::new();
    for node in context.catalog.partition_tree(&statement.table)? {
        let Some(name) = planned
            .get(&node.table)
            .filter(|_| !adopted.contains(&node.parent))
        else {
            adopted.insert(node.table);
            continue;
        };
        let hierarchy = context.catalog.table_hierarchy(&node.table)?;
        if hierarchy.partition_spec.is_some() {
            let mut partition = statement.clone();
            partition.table.clone_from(&node.table);
            validate_unique_partition_columns(&partition, &hierarchy)?;
            continue;
        }
        unique_build::validate_unique_index_build(
            unique_build::UniqueBuildContext {
                reads: context.reads,
                expressions: context.expressions,
                memory: context.memory,
                description: context.description,
            },
            &unique_build::UniqueIndexBuild {
                table: &node.table,
                name,
                keys: &statement.columns,
                key_types,
                predicate: statement.predicate.as_deref(),
                nulls_not_distinct: statement.nulls_not_distinct,
            },
        )?;
    }
    Ok(())
}

mod binding;
pub(crate) mod build_keys;
pub(crate) use build_keys::IndexBuildKeys;
pub mod constraint_names;
pub mod creation;
#[cfg(test)]
mod declaration_tests;
pub mod relocation;
pub mod renaming;

pub mod registration;
pub mod registry;
pub mod removal;
pub mod restoration;
pub mod routines;
pub mod unique_build;
