//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Predicate-aware unique index probes and runtime key values.

use super::ConstraintContext;
use uqa_sql::catalog::index::EnforcedKey;

use uqa_core::{DocId, Value};
use uqa_sql::{ast::Expr, SQLError};
use uqa_storage::document_store::Document;

#[derive(Clone, Copy)]
pub struct IndexExpressionContext<'a> {
    pub catalog: &'a dyn uqa_sql::semantics::conflict::ConflictCatalog,
    pub expressions: &'a dyn uqa_sql::semantics::partition::PartitionExpressions,
}
impl<'a> ConstraintContext<'a> {
    pub fn index_expressions(self) -> IndexExpressionContext<'a> {
        IndexExpressionContext {
            catalog: self.catalog,
            expressions: self.partitions.expressions,
        }
    }
}

pub fn index_predicate_accepts(
    context: IndexExpressionContext<'_>,
    table: &str,
    predicate: Option<&Expr>,
    document: &Document,
) -> Result<bool, SQLError> {
    let Some(predicate) = predicate else {
        return Ok(true);
    };
    let columns = context
        .catalog
        .try_describe_table(table)
        .map_err(|error| SQLError::Internal(format!("index predicate columns: {error}")))?
        .ok_or_else(|| SQLError::UnknownTable(table.into()))?;
    let schema = crate::RowSchema::with_types(
        columns.iter().map(|column| column.name.clone()).collect(),
        columns
            .iter()
            .map(|column| Some(column.ty.clone()))
            .collect(),
    );
    let result = context
        .expressions
        .evaluate_row(predicate, document, &schema, &[])?;
    Ok(matches!(result, Value::Bool(true)))
}

pub fn index_key_values(
    context: IndexExpressionContext<'_>,
    table: &str,
    keys: &[uqa_sql::ast::IndexKey],
    document: &Document,
) -> Result<Vec<Value>, SQLError> {
    if keys.iter().all(|key| key.column().is_some()) {
        return Ok(keys
            .iter()
            .map(|key| {
                document
                    .get(key.column().expect("column key"))
                    .cloned()
                    .unwrap_or(Value::Null)
            })
            .collect());
    }
    let columns = context
        .catalog
        .try_describe_table(table)
        .map_err(|error| SQLError::Internal(error.to_string()))?
        .ok_or_else(|| SQLError::UnknownTable(table.into()))?;
    let schema = crate::RowSchema::with_types(
        columns.iter().map(|column| column.name.clone()).collect(),
        columns
            .iter()
            .map(|column| Some(column.ty.clone()))
            .collect(),
    );
    keys.iter()
        .map(|key| match key {
            uqa_sql::ast::IndexKey::Column(column) => {
                Ok(document.get(column).cloned().unwrap_or(Value::Null))
            }
            uqa_sql::ast::IndexKey::Expression(expression) => {
                context
                    .expressions
                    .evaluate_row(expression, document, &schema, &[])
            }
        })
        .collect()
}

pub trait EnforcedKeyExecution {
    fn values(
        &self,
        context: ConstraintContext<'_>,
        table: &str,
        document: &Document,
    ) -> Result<Option<Vec<Value>>, SQLError>;
    fn find_conflict(
        &self,
        context: ConstraintContext<'_>,
        table: &str,
        values: &[Value],
        ignored: Option<DocId>,
    ) -> Result<Option<DocId>, SQLError>;
}
impl EnforcedKeyExecution for EnforcedKey {
    fn values(
        &self,
        context: ConstraintContext<'_>,
        table: &str,
        document: &Document,
    ) -> Result<Option<Vec<Value>>, SQLError> {
        if !index_predicate_accepts(
            context.index_expressions(),
            table,
            self.predicate.as_deref(),
            document,
        )? {
            return Ok(None);
        }
        let values = index_key_values(context.index_expressions(), table, &self.keys, document)?;
        if self.kind == uqa_sql::ast::TableKeyConstraintKind::Unique
            && !self.nulls_not_distinct
            && values.iter().any(|value| matches!(value, Value::Null))
        {
            return Ok(None);
        }
        Ok(Some(values))
    }

    fn find_conflict(
        &self,
        context: ConstraintContext<'_>,
        table: &str,
        values: &[Value],
        ignored: Option<DocId>,
    ) -> Result<Option<DocId>, SQLError> {
        if self.keys.iter().any(|key| key.column().is_none()) {
            let identity = self.index.as_ref().ok_or_else(|| {
                SQLError::Internal("expression index has no physical identity".into())
            })?;
            let key = uqa_storage::ValueIndexKey::Index(identity.qualified_name());
            let indexed = context
                .indexes
                .value_index_scan_key(
                    table,
                    &key,
                    &uqa_core::Predicate::Equals(Value::Row(values.to_vec())),
                )?
                .ok_or_else(|| {
                    SQLError::Internal(format!("missing physical index {identity:?}"))
                })?;
            let changes = context
                .reads
                .command_overlay_changed_ids(table)?
                .unwrap_or_default();
            for entry in indexed.entries() {
                let id = entry.doc_id;
                if Some(id) == ignored || changes.contains(&id) {
                    continue;
                }
                if context.reads.get_document(table, id)?.is_some() {
                    return Ok(Some(id));
                }
            }
            for id in changes.iter() {
                if Some(*id) == ignored {
                    continue;
                }
                if let Some(document) = context.reads.get_document(table, *id)? {
                    if self.values(context, table, &document)?.as_deref() == Some(values) {
                        return Ok(Some(*id));
                    }
                }
            }
            return Ok(None);
        }
        if self.predicate.is_none() {
            return context
                .indexes
                .find_conflict(table, &self.columns, values)
                .map(|id| id.filter(|id| Some(*id) != ignored));
        }
        let indexed = self
            .columns
            .first()
            .zip(values.first())
            .map(|(column, value)| {
                context.indexes.value_index_scan_key(
                    table,
                    &uqa_storage::ValueIndexKey::Column(column.clone()),
                    &if matches!(value, Value::Null) {
                        uqa_core::Predicate::IsNull
                    } else {
                        uqa_core::Predicate::Equals(value.clone())
                    },
                )
            })
            .transpose()?
            .flatten();
        let mut ids = if let Some(indexed) = indexed {
            indexed
                .entries()
                .iter()
                .map(|entry| entry.doc_id)
                .collect::<std::collections::BTreeSet<_>>()
        } else {
            context
                .reads
                .live_table_doc_ids(table)?
                .into_iter()
                .collect()
        };
        if let Some(changes) = context.reads.command_overlay_changed_ids(table)? {
            ids.extend(changes);
        }
        for id in ids {
            if Some(id) == ignored {
                continue;
            }
            let Some(document) = context.reads.get_document(table, id)? else {
                continue;
            };
            if self.values(context, table, &document)?.as_deref() == Some(values) {
                return Ok(Some(id));
            }
        }
        Ok(None)
    }
}
