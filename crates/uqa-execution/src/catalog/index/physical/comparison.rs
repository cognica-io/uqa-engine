//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Validate B-tree key comparisons before publishing a candidate row.

use super::{index_key_values, index_predicate_accepts, PhysicalIndexDefinitions};
use crate::mutation::constraints::{index_keys::key_values_equal, ConstraintContext};
use std::collections::BTreeSet;
use uqa_core::{DocId, Predicate, Value};
use uqa_sql::{ast::IndexKey, SQLError};
use uqa_storage::{document_store::Document, ValueIndexKey};

impl PhysicalIndexDefinitions {
    pub fn validate_key_comparisons(
        &self,
        context: ConstraintContext<'_>,
        table: &str,
        document: &Document,
        ignored_doc_id: Option<DocId>,
    ) -> Result<(), SQLError> {
        for ((_, physical_key), index) in &self.indexes {
            if index.table != table
                || !index.method.eq_ignore_ascii_case("btree")
                || !index
                    .definition
                    .key_types
                    .iter()
                    .any(uqa_sql::expr::type_comparison_can_fail)
            {
                continue;
            }
            let predicate = index.definition.predicate.as_deref();
            let expressions = context.index_expressions();
            if !index_predicate_accepts(expressions, table, predicate, document)? {
                continue;
            }
            let values = index_key_values(expressions, table, &index.keys, document)?;
            let (key, probe) = if index.keys.iter().any(|key| key.column().is_none()) {
                (
                    ValueIndexKey::Index(physical_key.clone()),
                    Predicate::Equals(Value::Row(values.clone())),
                )
            } else {
                let Some(IndexKey::Column(first)) = index.keys.first() else {
                    continue;
                };
                let probe = match &values[0] {
                    Value::Null => Predicate::IsNull,
                    value => Predicate::Equals(value.clone()),
                };
                (ValueIndexKey::Column(first.clone()), probe)
            };
            // Schema validation compares an existing row only with other rows. An index probe would compare the row with its own entry before that identity could be excluded.
            let mut candidates: BTreeSet<DocId> = if ignored_doc_id.is_some() {
                context
                    .reads
                    .live_table_doc_ids(table)?
                    .into_iter()
                    .collect()
            } else {
                match context.indexes.value_index_scan_key(table, &key, &probe)? {
                    Some(list) => list.entries().iter().map(|entry| entry.doc_id).collect(),
                    None => context
                        .reads
                        .live_table_doc_ids(table)?
                        .into_iter()
                        .collect(),
                }
            };
            if let Some(changes) = context.reads.command_overlay_changed_ids(table)? {
                candidates.extend(changes);
            }
            for id in candidates {
                if ignored_doc_id == Some(id) {
                    continue;
                }
                let Some(stored) = context.reads.get_document(table, id)? else {
                    continue;
                };
                if index_predicate_accepts(expressions, table, predicate, &stored)? {
                    key_values_equal(
                        &index_key_values(expressions, table, &index.keys, &stored)?,
                        &values,
                    )?;
                }
            }
        }
        Ok(())
    }
}
