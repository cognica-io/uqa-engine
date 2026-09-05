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

pub(in crate::sql) fn index_predicate_accepts(
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
        Ok(super::key_constraint_values(&self.constraint, document))
    }

    pub(in crate::sql) fn find_conflict(
        &self,
        engine: &Engine,
        table: &str,
        values: &[Value],
        ignored: Option<DocId>,
    ) -> Result<Option<DocId>, SQLError> {
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
