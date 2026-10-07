//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Scalar access candidates preserve the whole residual predicate and source column mapping.

use super::indexed_filter::{self, QueryIndexRead};
use uqa_core::{CancellationToken, DocId};
use uqa_sql::{
    ast::ColumnDef,
    retrieval::{RetrievalConstants, RetrievalExpr},
    SQLError, SQLParam, ScalarExpr,
};

pub(crate) struct IndexCandidates<'a> {
    pub reads: &'a dyn QueryIndexRead,
    pub table: &'a str,
    pub columns: &'a [ColumnDef],
    /// The source's visible name for a stored column, including a column alias.
    pub visible: &'a dyn Fn(&str) -> String,
    pub params: &'a [SQLParam],
    pub command_visible: bool,
    pub cancellation: &'a CancellationToken,
}

impl IndexCandidates<'_> {
    pub(crate) fn select(&self, filter: &ScalarExpr) -> Result<Option<Vec<DocId>>, SQLError> {
        self.cancellation.check()?;
        if let ScalarExpr::And(parts) = filter {
            // One answerable conjunct bounds the scan. The whole filter remains a residual, including other conjuncts and comparisons that can fail.
            for part in parts {
                if let Some(ids) = self.select(part)? {
                    return Ok(Some(ids));
                }
            }
            return Ok(None);
        }
        let lowered = uqa_sql::retrieval::lower_where(
            filter,
            &RetrievalConstants {
                params: self.params,
                evaluate: &|expression, params| {
                    if !uqa_sql::semantics::mutation_inputs::expr_is_row_independent(expression) {
                        return Err(SQLError::Internal("nonconstant index bound".into()));
                    }
                    crate::eval_scalar(expression, &crate::ScalarEvalContext::new(None, params))
                },
                stores: &|name| {
                    self.columns
                        .iter()
                        .any(|column| (self.visible)(&column.name) == name)
                },
            },
        );
        let Some(RetrievalExpr::Filter {
            field,
            predicate,
            source: None,
        }) = lowered
        else {
            return Ok(None);
        };
        let Some(column) = self
            .columns
            .iter()
            .find(|column| (self.visible)(&column.name) == field)
        else {
            return Ok(None);
        };
        let indexed = if self.command_visible {
            indexed_filter::scan(
                self.reads,
                self.table,
                &column.name,
                &predicate,
                self.cancellation,
            )
        } else {
            self.reads
                .value_index_scan(self.table, &column.name, &predicate)
        };
        indexed.map(|result| {
            result.map(|posting| posting.entries().iter().map(|entry| entry.doc_id).collect())
        })
    }
}

#[cfg(test)]
mod tests;
