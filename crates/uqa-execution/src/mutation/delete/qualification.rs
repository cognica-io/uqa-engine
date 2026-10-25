//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! DELETE candidate qualification and tuple-local rechecks.
use crate::mutation::rows::join_rows as dml_join_rows;
use crate::mutation::{
    constraints::context::MutationRead,
    rows::context::{MutationExpressionContext, MutationRowContext},
};
use crate::query::CteScope;
use uqa_core::DocId;
use uqa_sql::{plan::DeletePlan, SQLError, SQLParam};
use uqa_storage::document_store::Document;
pub(super) struct DeleteCandidateRecheck<'a, 'services, S: Clone + 'static> {
    pub(super) reads: &'services dyn MutationRead,
    pub(super) rows: MutationRowContext<'services>,
    pub(super) expressions: MutationExpressionContext<'services, S>,
    pub(super) stmt: &'a DeletePlan,
    pub(super) storage_table: &'a str,
    pub(super) params: &'a [SQLParam],
    pub(super) ctes: &'a CteScope<S>,
    pub(super) doc_id: DocId,
    pub(super) source_context: Option<&'a crate::OwnedPhysicalRow>,
}

pub(super) fn recheck_delete_candidate<S: Clone + Send + Sync + 'static>(
    context: DeleteCandidateRecheck<'_, '_, S>,
) -> Result<Option<(Document, Option<crate::OwnedPhysicalRow>)>, SQLError> {
    let DeleteCandidateRecheck {
        reads,
        rows,
        expressions,
        stmt,
        storage_table,
        params,
        ctes,
        doc_id,
        source_context,
    } = context;
    let Some(doc) = reads.get_document(storage_table, doc_id)? else {
        return Ok(None);
    };
    let target_row = crate::mutation::rows::target_row_for_storage(
        rows,
        &stmt.table,
        storage_table,
        &stmt.target_qualifier,
        doc_id,
        &doc,
    )?;
    let joined = source_context
        .map(|source_context| dml_join_rows(&target_row, source_context))
        .unwrap_or(target_row);
    let qualifies = stmt.predicate.as_ref().map_or(Ok(true), |filter| {
        crate::mutation::expressions::eval_mutation_expr(
            expressions,
            ctes,
            filter,
            Some(&joined),
            params,
        )
        .map(|value| uqa_sql::expr::truthy(&value))
    })?;
    Ok(qualifies.then(|| (doc, source_context.cloned())))
}

/// A plain DELETE has one target input; USING exposes every joined input, including duplicate matches, before mutation identity de-duplication.
pub(super) enum DeleteSourceRows {
    Plain(bool),
    Joined(crate::spill::SpillRows<crate::spill::SharedSpillReader>),
}
impl DeleteSourceRows {
    pub(super) fn new(rows: Option<&crate::SharedSpill>) -> Result<Self, SQLError> {
        rows.map_or(Ok(Self::Plain(true)), |rows| {
            rows.read_rows()
                .map(Self::Joined)
                .map_err(crate::physical::physical_exec_error)
        })
    }
}
impl Iterator for DeleteSourceRows {
    type Item = Result<Option<crate::OwnedPhysicalRow>, SQLError>;
    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::Plain(remaining) => std::mem::take(remaining).then_some(Ok(None)),
            Self::Joined(rows) => rows
                .next()
                .map(|row| row.map(Some).map_err(crate::physical::physical_exec_error)),
        }
    }
}
