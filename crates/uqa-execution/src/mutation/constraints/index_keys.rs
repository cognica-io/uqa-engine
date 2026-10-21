//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Predicate-aware unique index probes and runtime key values.

use super::ConstraintContext;
use crate::mutation::{
    candidate_ids::{visit_identity_pages, CandidateIds, CapturedCandidateIds},
    row_membership::PhysicalRowBuffer,
};
use uqa_sql::catalog::index::EnforcedKey;

use uqa_core::{DocId, Value};
use uqa_sql::{ast::Expr, SQLError};
use uqa_storage::document_store::Document;

#[derive(Clone, Copy)]
pub struct IndexExpressionContext<'a> {
    pub catalog: &'a dyn uqa_sql::semantics::conflict::ConflictCatalog,
    pub values: Option<&'a (dyn uqa_sql::expr::SQLValueCatalog + Send + Sync)>,
    pub expressions: &'a dyn uqa_sql::semantics::partition::PartitionExpressions,
}
impl<'a> ConstraintContext<'a> {
    pub fn index_expressions(self) -> IndexExpressionContext<'a> {
        IndexExpressionContext {
            catalog: self.catalog,
            values: Some(self.values),
            expressions: self.partitions.expressions,
        }
    }
}

/// A failed read of the changes a command staged.
pub(crate) fn changes_error(error: uqa_storage::StorageBackendError) -> SQLError {
    crate::storage_errors::storage_error("read command changes", &error)
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
        .map_err(SQLError::Internal)?
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
        if let Some(ignored) =
            ignored.filter(|_| values.iter().any(uqa_sql::expr::value_comparison_can_fail))
        {
            return find_conflict_excluding(self, context, table, values, ignored);
        }
        // A catalog-backed composite key has its complete tuple in both the stored index and the command cache. Column-only probes would repeatedly read every row sharing a prefix. Descriptors not yet attached to an index retain the evaluated column path.
        if self.keys.iter().any(|key| key.column().is_none())
            || (self.keys.len() > 1 && self.index_catalog.is_some())
        {
            return find_physical_conflict(self, context, table, values, ignored);
        }
        if self.predicate.is_none() {
            return context
                .indexes
                .find_conflict(table, &self.columns, values)
                .map(|id| id.filter(|id| Some(*id) != ignored));
        }
        find_partial_conflict(self, context, table, values, ignored)
    }
}

pub(crate) fn capture_index_candidates(
    context: ConstraintContext<'_>,
    table: &str,
    key: &uqa_storage::ValueIndexKey,
    predicate: &uqa_core::Predicate,
) -> Result<Option<CapturedCandidateIds>, SQLError> {
    let mut ids = CandidateIds::new(
        context.memory.work_mem_bytes()?,
        context.cancellation.clone(),
    );
    if !context
        .indexes
        .visit_value_index_key(table, key, predicate, &mut |id| ids.push(0, id))?
    {
        return Ok(None);
    }
    ids.finish().map(Some)
}

fn find_physical_conflict(
    key: &EnforcedKey,
    context: ConstraintContext<'_>,
    table: &str,
    values: &[Value],
    ignored: Option<DocId>,
) -> Result<Option<DocId>, SQLError> {
    let key = local_physical_key(key, context, table)?;
    let indexed = capture_index_candidates(
        context,
        table,
        &key,
        &uqa_core::Predicate::Equals(Value::Row(values.to_vec().into())),
    )?
    .ok_or_else(|| SQLError::Internal(format!("missing physical index {key:?}")))?;
    let uqa_storage::ValueIndexKey::Index(physical_key) = &key else {
        unreachable!("tuple index has a physical identity")
    };
    let mut staged = CandidateIds::new(
        context.memory.work_mem_bytes()?,
        context.cancellation.clone(),
    );
    let changes = context.indexes.visit_staged_expression_matches(
        table,
        physical_key,
        values,
        &mut |id| staged.push(0, id),
    )?;
    let staged = staged.finish()?;
    for row in indexed.read_rows()? {
        let (_, id) = row?;
        if Some(id) != ignored
            && !changes.contains_change(id).map_err(changes_error)?
            && context.reads.get_document(table, id)?.is_some()
        {
            return Ok(Some(id));
        }
    }
    // Command-frame order, including memory and spilled entries, is retained rather than sorting this second probe.
    for row in staged.read_rows()? {
        let (_, id) = row?;
        if Some(id) != ignored {
            return Ok(Some(id));
        }
    }
    Ok(None)
}

fn find_partial_conflict(
    key: &EnforcedKey,
    context: ConstraintContext<'_>,
    table: &str,
    values: &[Value],
    ignored: Option<DocId>,
) -> Result<Option<DocId>, SQLError> {
    let mut ids = PhysicalRowBuffer::new(
        context.memory.work_mem_bytes()?,
        context.cancellation.clone(),
        "partial index candidates",
    );
    let indexed = match key.columns.first().zip(values.first()) {
        Some((column, value)) => context.indexes.visit_value_index_key(
            table,
            &uqa_storage::ValueIndexKey::Column(column.clone()),
            &if matches!(value, Value::Null) {
                uqa_core::Predicate::IsNull
            } else {
                uqa_core::Predicate::Equals(value.clone())
            },
            &mut |id| ids.insert("", id),
        )?,
        None => false,
    };
    if !indexed {
        visit_identity_pages(
            context.cancellation,
            |after, limit, control| {
                context
                    .reads
                    .live_table_doc_id_page(table, after, limit, control)
            },
            &mut |id| ids.insert("", id),
        )?;
    }
    context
        .indexes
        .visit_staged_matches(table, &key.columns, values, &mut |id| ids.insert("", id))?;
    let ids = ids.finish()?;
    let mut found = None;
    ids.visit(&mut |_, id| {
        if Some(id) != ignored {
            if let Some(document) = context.reads.get_document(table, id)? {
                if let Some(actual) = key.values(context, table, &document)? {
                    if key_values_equal(&actual, values, context.values)? {
                        found = Some(id);
                        return Ok(false);
                    }
                }
            }
        }
        Ok(true)
    })?;
    Ok(found)
}

fn find_conflict_excluding(
    key: &EnforcedKey,
    context: ConstraintContext<'_>,
    table: &str,
    values: &[Value],
    ignored: DocId,
) -> Result<Option<DocId>, SQLError> {
    // A probe would compare the excluded row with its own key before applying its identity filter. Only other visible rows participate in this uniqueness check; rewrite validation separately compares any retained old index entry.
    let ids = crate::mutation::candidate_ids::CandidateIds::from_pages(
        context.memory.work_mem_bytes()?,
        context.cancellation,
        |after, limit, control| {
            context
                .reads
                .live_table_doc_id_page(table, after, limit, control)
        },
    )?;
    for row in ids.read_rows()? {
        let (_, id) = row?;
        if id == ignored {
            continue;
        }
        let Some(document) = context.reads.get_document(table, id)? else {
            continue;
        };
        if let Some(actual) = key.values(context, table, &document)? {
            if key_values_equal(&actual, values, context.values)? {
                return Ok(Some(id));
            }
        }
    }
    Ok(None)
}

pub(crate) fn key_values_equal(
    left: &[Value],
    right: &[Value],
    catalog: &dyn uqa_sql::expr::SQLValueCatalog,
) -> Result<bool, SQLError> {
    for (left, right) in left.iter().zip(right) {
        if !uqa_sql::expr::compare_typed_values_with_enum_catalog(
            left,
            right,
            &uqa_core::memory::ProductionControl::uncontrolled(),
            Some(catalog),
            None,
        )?
        .is_eq()
        {
            return Ok(false);
        }
    }
    Ok(left.len() == right.len())
}

fn local_physical_key(
    index: &EnforcedKey,
    context: ConstraintContext<'_>,
    table: &str,
) -> Result<uqa_storage::ValueIndexKey, SQLError> {
    let identity = index
        .index_catalog
        .as_ref()
        .ok_or_else(|| SQLError::Internal("unique index has no physical identity".into()))?;
    let local_keys = context
        .catalog
        .enforced_keys(table)
        .map_err(SQLError::Internal)?;
    let identity = local_keys
        .iter()
        .find(|key| {
            key.index_catalog.as_ref().is_some_and(|candidate| {
                candidate.identity.object_id == identity.identity.object_id
            }) || key.index_ancestors.contains(&identity.identity.object_id)
        })
        .and_then(|key| key.index_catalog.as_ref())
        .ok_or_else(|| SQLError::Internal("partition arbiter has no local index".into()))?;
    Ok(uqa_storage::ValueIndexKey::Index(
        identity.physical_key.clone(),
    ))
}
