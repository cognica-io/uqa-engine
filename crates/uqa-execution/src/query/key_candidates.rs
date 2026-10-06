//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The rows a `WHERE` clause confines to named document identities: by comparing the engine's `_doc_id`, or a single integer primary key whose values are its rows' identities, with values that read no row.

use std::collections::BTreeSet;

use uqa_core::{DocId, Value};
use uqa_sql::{ast::BinaryOp, ast::ColumnDef, SQLParam, ScalarExpr};

/// The columns of a table whose values name its rows' identities.
#[derive(Clone, Copy)]
pub(crate) struct IdentityColumns<'a> {
    /// Whether `_doc_id` names the engine's identity, which it does unless the table has a column of that name.
    document_id: bool,
    /// The name, as the filter writes it, of the table's single integer primary key when its values are the rows' identities.
    key: Option<&'a str>,
}

impl<'a> IdentityColumns<'a> {
    /// The identity columns of a table with `columns`, whose integer primary-key values name its rows' identities when `maps_integer_keys`; `visible` gives the name a filter writes for a column.
    pub(crate) fn new(
        columns: &'a [ColumnDef],
        maps_integer_keys: bool,
        visible: impl FnOnce(&'a str) -> &'a str,
    ) -> Self {
        Self {
            document_id: !columns
                .iter()
                .any(|column| column.name == uqa_sql::semantics::DOC_ID_COLUMN),
            key: maps_integer_keys
                .then(|| {
                    columns
                        .iter()
                        .find(|column| column.primary_key && column.ty.is_integer())
                })
                .flatten()
                .map(|column| visible(column.name.as_str())),
        }
    }
}

/// The identities a filter confines its rows to, in ascending order: an equality or an `IN` list that compares `_doc_id` or the integer primary key with values that read no row, alone or as a conjunct. A row at any other identity fails the filter, so a read can visit only these and evaluate the whole filter on them. `None` when the filter names no such set; a key value that names no identity leaves its conjunct unrestricted, since such a row has an identity its key does not name.
pub(crate) fn key_candidates(
    filter: &ScalarExpr,
    params: &[SQLParam],
    columns: IdentityColumns<'_>,
) -> Option<Vec<DocId>> {
    candidates(filter, params, columns).map(|identities| identities.into_iter().collect())
}

fn candidates(
    filter: &ScalarExpr,
    params: &[SQLParam],
    columns: IdentityColumns<'_>,
) -> Option<BTreeSet<DocId>> {
    if let Some(membership) = uqa_sql::semantics::membership_operands(filter) {
        if membership.negated {
            return None;
        }
        let column = identity_column(membership.value, columns)?;
        let mut identities = BTreeSet::new();
        for item in membership.items.iter() {
            let value = match item {
                uqa_sql::semantics::MembershipItem::Expression(expression) => {
                    constant(expression, params)?
                }
                uqa_sql::semantics::MembershipItem::Constant(value) => value.clone(),
            };
            identities.extend(column.identities(&value)?);
        }
        return Some(identities);
    }
    match filter {
        ScalarExpr::And(parts) => parts
            .iter()
            .filter_map(|part| candidates(part, params, columns))
            .reduce(|left, right| left.intersection(&right).copied().collect()),
        ScalarExpr::Binary {
            op: BinaryOp::Equal,
            lhs,
            rhs,
        } => {
            let (column, value) =
                match (identity_column(lhs, columns), identity_column(rhs, columns)) {
                    (Some(column), None) => (column, constant(rhs, params)?),
                    (None, Some(column)) => (column, constant(lhs, params)?),
                    _ => return None,
                };
            column
                .identities(&value)
                .map(|identities| identities.into_iter().collect())
        }
        _ => None,
    }
}

/// A column whose values name identities.
#[derive(Clone, Copy)]
enum IdentityColumn {
    DocumentId,
    Key,
}

impl IdentityColumn {
    /// The identities a value of the column names: for `_doc_id`, an integer names itself and a negative integer names none, since no row has a negative identity; for the key, an integer within the identity range names its identity. `None` for any other value, whose comparison the filter itself decides.
    fn identities(self, value: &Value) -> Option<Vec<DocId>> {
        match (self, value) {
            (Self::DocumentId, Value::Int(value)) => {
                Some(DocId::try_from(*value).into_iter().collect())
            }
            (Self::Key, value) => {
                uqa_sql::semantics::key_identity::key_document_id(value).map(|id| vec![id])
            }
            _ => None,
        }
    }
}

fn identity_column(
    expression: &ScalarExpr,
    columns: IdentityColumns<'_>,
) -> Option<IdentityColumn> {
    let (ScalarExpr::Column(column) | ScalarExpr::QualifiedColumn { column, .. }) = expression
    else {
        return None;
    };
    if columns.document_id && column == uqa_sql::semantics::DOC_ID_COLUMN {
        Some(IdentityColumn::DocumentId)
    } else if columns.key == Some(column.as_str()) {
        Some(IdentityColumn::Key)
    } else {
        None
    }
}

/// The value of an expression that reads no row and calls no function.
fn constant(expression: &ScalarExpr, params: &[SQLParam]) -> Option<Value> {
    if !uqa_sql::semantics::mutation_inputs::expr_is_row_independent(expression) {
        return None;
    }
    crate::eval_scalar(expression, &crate::ScalarEvalContext::new(None, params)).ok()
}

#[cfg(test)]
mod tests;
