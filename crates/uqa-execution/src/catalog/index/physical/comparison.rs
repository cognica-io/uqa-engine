//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Validate B-tree key comparisons before publishing a candidate row.

use super::{index_key_values, index_predicate_accepts, PhysicalIndexDefinitions};
use crate::mutation::candidate_ids::{CandidateIds, CapturedCandidateIds};
use crate::mutation::constraints::{
    index_keys::{capture_index_candidates, changes_error, key_values_equal},
    ConstraintContext,
};
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
                    Predicate::Equals(Value::Row(values.clone().into())),
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
            let candidates = comparison_candidates(context, table, &key, &probe, ignored_doc_id)?;
            let changes = context
                .reads
                .command_overlay_changes(table)?
                .unwrap_or_default();
            let mut changed = changes.changes().peekable();
            let mut candidates = candidates.read_rows()?.peekable();
            // The candidates and the changed rows in identity order, each identity once.
            loop {
                let next_changed = match changed.peek() {
                    Some(Ok((id, _))) => Some(*id),
                    Some(Err(_)) => {
                        let Some(Err(error)) = changed.next() else {
                            unreachable!("a peeked failure");
                        };
                        return Err(changes_error(error));
                    }
                    None => None,
                };
                let next_candidate = match candidates.peek() {
                    Some(Ok((_, id))) => Some(*id),
                    Some(Err(_)) => {
                        return Err(candidates.next().expect("peeked candidate").unwrap_err());
                    }
                    None => None,
                };
                let id = match (next_candidate, next_changed) {
                    (None, None) => break,
                    (Some(candidate), Some(changed_id)) if changed_id < candidate => {
                        changed.next();
                        changed_id
                    }
                    (Some(candidate), next_changed) => {
                        candidates.next();
                        if next_changed == Some(candidate) {
                            changed.next();
                        }
                        candidate
                    }
                    (None, Some(changed_id)) => {
                        changed.next();
                        changed_id
                    }
                };
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
                        context.values,
                    )?;
                }
            }
        }
        Ok(())
    }
}

fn comparison_candidates(
    context: ConstraintContext<'_>,
    table: &str,
    key: &ValueIndexKey,
    probe: &Predicate,
    ignored: Option<DocId>,
) -> Result<CapturedCandidateIds, SQLError> {
    // Schema validation compares an existing row only with other rows. An index probe would compare the row with its own entry before that identity could be excluded.
    if ignored.is_none() {
        if let Some(candidates) = capture_index_candidates(context, table, key, probe)? {
            return Ok(candidates);
        }
    }
    CandidateIds::from_pages(
        context.memory.work_mem_bytes()?,
        context.cancellation,
        |after, limit, control| {
            context
                .reads
                .live_table_doc_id_page(table, after, limit, control)
        },
    )
}
