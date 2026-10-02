//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The rows a `WHERE` clause confines to named document identities.

use std::collections::BTreeSet;

use uqa_core::{DocId, Value};
use uqa_sql::{ast::BinaryOp, ast::ColumnDef, SQLParam, ScalarExpr};

/// The identities a filter confines its rows to by comparing the engine's `_doc_id` with constants, in ascending order: an equality or an `IN` list, alone or as a conjunct. A row at any other identity fails the filter, so a read can visit only these and evaluate the whole filter on them. `None` when the filter names no such set, or when the table declares a `_doc_id` column of its own, which the name then means.
pub(crate) fn document_id_candidates(
    filter: &ScalarExpr,
    params: &[SQLParam],
    columns: &[ColumnDef],
) -> Option<Vec<DocId>> {
    if columns
        .iter()
        .any(|column| column.name == uqa_sql::semantics::DOC_ID_COLUMN)
    {
        return None;
    }
    candidates(filter, params).map(|identities| identities.into_iter().collect())
}

fn candidates(filter: &ScalarExpr, params: &[SQLParam]) -> Option<BTreeSet<DocId>> {
    match filter {
        ScalarExpr::And(parts) => parts
            .iter()
            .filter_map(|part| candidates(part, params))
            .reduce(|left, right| left.intersection(&right).copied().collect()),
        ScalarExpr::Binary {
            op: BinaryOp::Equal,
            lhs,
            rhs,
        } => {
            let value = match (names_document_id(lhs), names_document_id(rhs)) {
                (true, false) => constant(rhs, params)?,
                (false, true) => constant(lhs, params)?,
                _ => return None,
            };
            Some(identity(&value)?.into_iter().collect())
        }
        ScalarExpr::InList {
            expr,
            list,
            negated: false,
        } if names_document_id(expr) => {
            let mut identities = BTreeSet::new();
            for item in list {
                identities.extend(identity(&constant(item, params)?)?);
            }
            Some(identities)
        }
        _ => None,
    }
}

fn names_document_id(expression: &ScalarExpr) -> bool {
    match expression {
        ScalarExpr::Column(column) | ScalarExpr::QualifiedColumn { column, .. } => {
            column == uqa_sql::semantics::DOC_ID_COLUMN
        }
        _ => false,
    }
}

/// The value of an expression that reads no row and calls no function.
fn constant(expression: &ScalarExpr, params: &[SQLParam]) -> Option<Value> {
    if !uqa_sql::semantics::mutation_inputs::expr_is_row_independent(expression) {
        return None;
    }
    crate::eval_scalar(expression, &crate::ScalarEvalContext::new(None, params)).ok()
}

/// The identities an integer names: itself, or none for a negative integer, since no row has a negative identity. `None` for a value of another type, whose comparison the filter itself decides.
fn identity(value: &Value) -> Option<Vec<DocId>> {
    match value {
        Value::Int(value) => Some(DocId::try_from(*value).into_iter().collect()),
        _ => None,
    }
}
