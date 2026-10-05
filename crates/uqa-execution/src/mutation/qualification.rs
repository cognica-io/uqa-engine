//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Tuple qualification evaluates retrieval support through the query executor and ordinary expressions through the scalar evaluator, preserving short-circuiting and the statement's subquery scope.

use std::cell::RefCell;

use uqa_core::{memory::MemoryBudget, DocId};
use uqa_sql::{SQLError, SQLParam, ScalarExpr};
use uqa_storage::read_control::StorageReadControl;

use super::statement::context::{with_mutation_snapshot, MutationStatementContext};
use crate::{query::CteScope, OwnedPhysicalRow};

mod support;
use support::{contains, retain_support, DocumentSupport};

struct Support {
    table: String,
    name: String,
    args: Vec<ScalarExpr>,
    rows: DocumentSupport,
}

#[derive(Default)]
struct Cache {
    control: Option<StorageReadControl>,
    support: Vec<Support>,
}

/// A command's stable retrieval leaves are executed against its retained read generation. Their exact document support spills under one shared allowance; scores stay owned by the retrieval executor and are not converted into SQL booleans.
pub(super) struct RowQualification<S> {
    snapshot: Option<S>,
    cache: RefCell<Cache>,
}

impl<S: Clone + Send + Sync + 'static> RowQualification<S> {
    pub(super) fn new(snapshot: Option<S>) -> Self {
        Self {
            snapshot,
            cache: RefCell::default(),
        }
    }

    pub(super) fn evaluate(
        &self,
        context: &MutationStatementContext<'_, S>,
        ctes: &CteScope<S>,
        predicate: &ScalarExpr,
        row: &OwnedPhysicalRow,
        identity: (&str, DocId),
        params: &[SQLParam],
    ) -> Result<bool, SQLError> {
        let (table, doc_id) = identity;
        let retrieval = |name: &str, args: &[ScalarExpr]| {
            let source = &context.query.source;
            let volatile = args.iter().any(|argument| {
                uqa_sql::semantics::volatility::expr_contains_volatile_function(
                    source.volatility,
                    argument,
                )
            });
            if !volatile {
                let cache = self.cache.borrow();
                if let Some(support) = cache.support.iter().find(|support| {
                    support.table == table && support.name == name && support.args == args
                }) {
                    return contains(
                        &support.rows,
                        doc_id,
                        cache.control.as_ref().expect("cached support control"),
                    );
                }
            }
            let execute = |selected: &MutationStatementContext<'_, S>| {
                selected
                    .query
                    .source
                    .relation_retrieval
                    .function(table, table, name, args, params, None)
            };
            let entries = match &self.snapshot {
                Some(snapshot) => with_mutation_snapshot(context.snapshots, snapshot, execute)?,
                None => execute(context)?,
            };
            if volatile {
                return Ok(entries.iter().any(|entry| entry.doc_id == doc_id));
            }
            let mut cache = self.cache.borrow_mut();
            if cache.control.is_none() {
                let runtime = source.relational.runtime;
                cache.control = Some(StorageReadControl::new(
                    &MemoryBudget::new(runtime.work_mem_bytes()?),
                    &runtime.cancellation_token(),
                ));
            }
            let control = cache.control.as_ref().expect("support control initialized");
            let rows = retain_support(entries, control)?;
            let matched = contains(&rows, doc_id, control)?;
            cache.support.push(Support {
                table: table.to_owned(),
                name: name.to_owned(),
                args: args.to_vec(),
                rows,
            });
            Ok(matched)
        };
        super::expressions::eval_mutation_expr_with_retrieval(
            context
                .mutation
                .preparation
                .referential
                .assignment
                .expressions,
            ctes,
            predicate,
            Some(row),
            params,
            Some(&retrieval),
        )
        .map(|value| uqa_sql::expr::truthy(&value))
    }
}

#[cfg(test)]
mod tests;
