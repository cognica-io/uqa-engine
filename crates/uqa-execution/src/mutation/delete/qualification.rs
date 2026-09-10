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

pub(super) struct QualifiedDeleteCandidate {
    pub(super) row: Option<(Document, Option<crate::OwnedPhysicalRow>)>,
    pub(super) qualification_count: usize,
}

pub(super) struct DeleteCandidateQualification<'a, 'services, S: Clone + 'static> {
    pub(super) reads: &'services dyn MutationRead,
    pub(super) rows: MutationRowContext<'services>,
    pub(super) expressions: MutationExpressionContext<'services, S>,
    pub(super) stmt: &'a DeletePlan,
    pub(super) storage_table: &'a str,
    pub(super) params: &'a [SQLParam],
    pub(super) ctes: &'a CteScope<S>,
    pub(super) using_rows: Option<&'a crate::SharedSpill>,
    pub(super) doc_id: DocId,
    pub(super) count_all_qualifications: bool,
}

pub(super) fn qualified_delete_candidate<S: Clone + Send + Sync + 'static>(
    context: DeleteCandidateQualification<'_, '_, S>,
) -> Result<QualifiedDeleteCandidate, SQLError> {
    let DeleteCandidateQualification {
        reads,
        rows,
        expressions,
        stmt,
        storage_table,
        params,
        ctes,
        using_rows,
        doc_id,
        count_all_qualifications,
    } = context;
    let Some(doc) = reads.get_document(storage_table, doc_id)? else {
        return Ok(QualifiedDeleteCandidate {
            row: None,
            qualification_count: 0,
        });
    };
    let target_row = crate::mutation::rows::target_row_for_storage(
        rows,
        &stmt.table,
        storage_table,
        &stmt.target_qualifier,
        doc_id,
        &doc,
    )?;
    match using_rows {
        None => {
            let qualifies = stmt.predicate.as_ref().map_or(Ok(true), |filter| {
                crate::mutation::expressions::eval_mutation_expr(
                    expressions,
                    ctes,
                    filter,
                    Some(&target_row),
                    params,
                )
                .map(|value| uqa_sql::expr::truthy(&value))
            })?;
            Ok(QualifiedDeleteCandidate {
                row: qualifies.then_some((doc, None)),
                qualification_count: usize::from(qualifies),
            })
        }
        Some(rows) => {
            let reader = rows
                .read_rows()
                .map_err(crate::query::projection::physical_exec_error)?;
            let mut first = None;
            let mut qualification_count = 0;
            for using_row in reader {
                let source_context =
                    using_row.map_err(crate::query::projection::physical_exec_error)?;
                let joined = dml_join_rows(&target_row, &source_context);
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
                if qualifies {
                    qualification_count += 1;
                    if first.is_none() {
                        first = Some(source_context);
                    }
                    if !count_all_qualifications {
                        break;
                    }
                }
            }
            Ok(QualifiedDeleteCandidate {
                row: first.map(|source_context| (doc, Some(source_context))),
                qualification_count,
            })
        }
    }
}

pub(super) fn count_delete_source_qualifications<S: Clone + Send + Sync + 'static>(
    expressions: MutationExpressionContext<'_, S>,
    stmt: &DeletePlan,
    ctes: &CteScope<S>,
    using_rows: &crate::SharedSpill,
    params: &[SQLParam],
) -> Result<usize, SQLError> {
    let mut count = 0;
    for source in using_rows
        .read_rows()
        .map_err(crate::query::projection::physical_exec_error)?
    {
        let source = source.map_err(crate::query::projection::physical_exec_error)?;
        let qualifies = stmt.predicate.as_ref().map_or(Ok(true), |predicate| {
            crate::mutation::expressions::eval_mutation_expr(
                expressions,
                ctes,
                predicate,
                Some(&source),
                params,
            )
            .map(|value| uqa_sql::expr::truthy(&value))
        })?;
        count += usize::from(qualifies);
    }
    Ok(count)
}
