//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Check one table's rows as building a unique index over them does.

use super::build_keys::IndexBuildKeys;
use crate::mutation::constraints::{
    context::{ConstraintContext, MutationRead},
    index_keys::{index_key_values, index_predicate_accepts, IndexExpressionContext},
};
use crate::query::runtime::QueryMemorySettings;
use uqa_core::Value;
use uqa_sql::{
    ast::{Expr, IndexKey},
    ColumnType, SQLError,
};

/// Describes the key values of an index for an error's detail, as `PostgreSQL`'s `BuildIndexValueDescription` does: `(names)=(values)`, or `None` when the current role may not see the key.
pub trait IndexKeyDescription {
    fn describe_index_key(
        &self,
        table: &str,
        keys: &[IndexKey],
        key_types: &[ColumnType],
        values: &[Value],
    ) -> Result<Option<String>, SQLError>;
}

/// The rows, expressions and sort memory that a unique index build reads.
#[derive(Clone, Copy)]
pub struct UniqueBuildContext<'a> {
    pub reads: &'a dyn MutationRead,
    pub expressions: IndexExpressionContext<'a>,
    pub memory: &'a dyn QueryMemorySettings,
    pub description: &'a dyn IndexKeyDescription,
}

impl<'a> UniqueBuildContext<'a> {
    pub fn of(constraints: &'a ConstraintContext<'a>) -> Self {
        Self {
            reads: constraints.reads,
            expressions: constraints.index_expressions(),
            memory: constraints.memory,
            description: constraints,
        }
    }
}

/// A unique index over the rows of one table.
pub struct UniqueIndexBuild<'a> {
    /// The table whose rows the index holds; a partitioned table's index is built on each leaf.
    pub table: &'a str,
    /// The index's name, which a repeated key reports.
    pub name: &'a str,
    pub keys: &'a [IndexKey],
    /// The output type of each key, which prints a repeated key.
    pub key_types: &'a [ColumnType],
    pub predicate: Option<&'a Expr>,
    pub nulls_not_distinct: bool,
}

/// `could not create unique index "x"` with the repeated key, or `Duplicate keys exist.` when the key may not be shown.
pub fn duplicated_index_key(name: &str, key: Option<String>) -> SQLError {
    SQLError::Diagnostic {
        sqlstate: "23505".into(),
        message: format!("could not create unique index \"{name}\""),
        detail: Some(key.map_or_else(
            || "Duplicate keys exist.".into(),
            |key| format!("Key {key} is duplicated."),
        )),
        hint: None,
    }
}

/// Read the index's keys from the rows of its table and fail on a repeated key as `PostgreSQL`'s btree build does.
pub fn validate_unique_index_build(
    context: UniqueBuildContext<'_>,
    build: &UniqueIndexBuild<'_>,
) -> Result<(), SQLError> {
    let mut keys = IndexBuildKeys::new(build.keys.len(), context.memory.work_mem_bytes()?);
    for id in context.reads.live_table_doc_ids(build.table)? {
        let document = context
            .reads
            .get_document(build.table, id)?
            .ok_or_else(|| SQLError::Internal("index build lost a visible row".into()))?;
        if !index_predicate_accepts(context.expressions, build.table, build.predicate, &document)? {
            continue;
        }
        keys.push(index_key_values(
            context.expressions,
            build.table,
            build.keys,
            &document,
        )?)?;
    }
    let Some(values) =
        keys.first_duplicate(true, build.nulls_not_distinct, context.expressions.values)?
    else {
        return Ok(());
    };
    Err(duplicated_index_key(
        build.name,
        context.description.describe_index_key(
            build.table,
            build.keys,
            build.key_types,
            &values,
        )?,
    ))
}
