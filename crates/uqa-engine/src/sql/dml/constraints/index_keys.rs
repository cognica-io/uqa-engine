//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Predicate-aware unique index probes and runtime key values.

use crate::engine_catalog_indexes::EnforcedKey;
use crate::Engine;
use uqa_core::{DocId, Value};
use uqa_sql::{ast::Expr, SQLError};
use uqa_storage::document_store::Document;

pub(crate) fn index_predicate_accepts(
    engine: &Engine,
    table: &str,
    predicate: Option<&Expr>,
    document: &Document,
) -> Result<bool, SQLError> {
    let Some(predicate) = predicate else {
        return Ok(true);
    };
    let columns = engine
        .try_describe_table(table)
        .map_err(|error| SQLError::Internal(format!("index predicate columns: {error}")))?
        .ok_or_else(|| SQLError::UnknownTable(table.into()))?;
    let schema = uqa_execution::RowSchema::with_types(
        columns.iter().map(|column| column.name.clone()).collect(),
        columns
            .iter()
            .map(|column| Some(column.ty.clone()))
            .collect(),
    );
    let result = crate::sql::scalar::eval_lowered_expression_with_schema(
        engine,
        predicate,
        document,
        &schema,
        &[],
    )?;
    Ok(matches!(result, Value::Bool(true)))
}

pub(crate) fn index_key_values(
    engine: &Engine,
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
    let columns = engine
        .try_describe_table(table)
        .map_err(|error| SQLError::Internal(error.to_string()))?
        .ok_or_else(|| SQLError::UnknownTable(table.into()))?;
    let schema = uqa_execution::RowSchema::with_types(
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
                crate::sql::scalar::eval_lowered_expression_with_schema(
                    engine,
                    expression,
                    document,
                    &schema,
                    &[],
                )
            }
        })
        .collect()
}

impl EnforcedKey {
    pub(in crate::sql) fn values(
        &self,
        engine: &Engine,
        table: &str,
        document: &Document,
    ) -> Result<Option<Vec<Value>>, SQLError> {
        if !index_predicate_accepts(engine, table, self.predicate.as_deref(), document)? {
            return Ok(None);
        }
        let values = index_key_values(engine, table, &self.keys, document)?;
        if self.kind == uqa_sql::ast::TableKeyConstraintKind::Unique
            && !self.nulls_not_distinct
            && values.iter().any(|value| matches!(value, Value::Null))
        {
            return Ok(None);
        }
        Ok(Some(values))
    }

    pub(in crate::sql) fn find_conflict(
        &self,
        engine: &Engine,
        table: &str,
        values: &[Value],
        ignored: Option<DocId>,
    ) -> Result<Option<DocId>, SQLError> {
        if self.keys.iter().any(|key| key.column().is_none()) {
            let identity = self.index.as_ref().ok_or_else(|| {
                SQLError::Internal("expression index has no physical identity".into())
            })?;
            let key = uqa_storage::ValueIndexKey::Index(identity.qualified_name());
            let indexed = engine
                .value_index_scan_key(
                    table,
                    &key,
                    &uqa_core::Predicate::Equals(Value::Row(values.to_vec())),
                )?
                .ok_or_else(|| {
                    SQLError::Internal(format!("missing physical index {identity:?}"))
                })?;
            let changes = engine.command_overlay_changes(table)?.unwrap_or_default();
            for entry in indexed.entries() {
                let id = entry.doc_id;
                if Some(id) == ignored || changes.contains_key(&id) {
                    continue;
                }
                if engine.get_document(table, id)?.is_some() {
                    return Ok(Some(id));
                }
            }
            for id in changes.keys() {
                if Some(*id) == ignored {
                    continue;
                }
                if let Some(document) = engine.get_document(table, *id)? {
                    if self.values(engine, table, &document)?.as_deref() == Some(values) {
                        return Ok(Some(*id));
                    }
                }
            }
            return Ok(None);
        }
        if self.predicate.is_none() {
            return engine
                .find_conflict(table, &self.columns, values)
                .map(|id| id.filter(|id| Some(*id) != ignored));
        }
        let indexed = self
            .columns
            .first()
            .zip(values.first())
            .map(|(column, value)| {
                engine.value_index_scan(
                    table,
                    column,
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
            engine.live_table_doc_ids(table)?.into_iter().collect()
        };
        if let Some(changes) = engine.command_overlay_changes(table)? {
            ids.extend(changes.into_keys());
        }
        for id in ids {
            if Some(id) == ignored {
                continue;
            }
            let Some(document) = engine.get_document(table, id)? else {
                continue;
            };
            if self.values(engine, table, &document)?.as_deref() == Some(values) {
                return Ok(Some(id));
            }
        }
        Ok(None)
    }
}
