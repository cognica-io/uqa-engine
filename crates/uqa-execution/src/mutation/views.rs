//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Physical row projection through automatically updatable view layers.
use super::rows::context::{MutationExpressionContext, MutationRowContext};
use crate::{query::CteScope, OwnedPhysicalRow, PhysicalRow};
use std::collections::BTreeSet;
use uqa_core::{DocId, Value};
use uqa_sql::{semantics::view_rewrite::context::ViewRewriteContext, RowSchema, SQLError};
use uqa_storage::document_store::Document;

#[derive(Clone)]
pub struct ViewRowContext<'a, S: Clone + 'static> {
    pub rewrite: ViewRewriteContext<'a>,
    pub volatility: &'a dyn uqa_sql::semantics::volatility::VolatilityCatalog,
    pub rows: MutationRowContext<'a>,
    pub expressions: MutationExpressionContext<'a, S>,
}
impl<S: Clone + 'static> Copy for ViewRowContext<'_, S> {}

mod projection;
pub use projection::automatic_view_rule_expressions;

pub struct AutomaticViewRuleDocument<'a, S: Clone + 'static> {
    pub context: ViewRowContext<'a, S>,
    pub view: &'a str,
    pub document_relation: Option<&'a str>,
    pub storage_table: Option<&'a str>,
    pub doc_id: Option<DocId>,
    pub document: &'a Document,
    pub required_columns: &'a BTreeSet<String>,
    pub params: &'a [uqa_sql::SQLParam],
    pub scope: &'a CteScope<S>,
}

pub fn automatic_view_rule_document<S: Clone + 'static>(
    request: AutomaticViewRuleDocument<'_, S>,
) -> Result<Document, SQLError> {
    let context = request.context;
    let params = request.params;
    let scope = request.scope;
    let projected = automatic_view_rule_expressions(request)?;
    projected
        .expressions
        .into_iter()
        .map(|(name, expression)| {
            crate::query::catalog_expression::eval_stored_expression_plan_with_row(
                context.expressions.expressions,
                scope.clone(),
                &expression,
                &projected.source.schema,
                &projected.source.row,
                params,
            )
            .map(|value| (name, value))
        })
        .collect()
}

pub fn stored_view_document_row(
    context: MutationRowContext<'_>,
    view: &str,
    qualifier: &str,
    document: &Document,
) -> Result<OwnedPhysicalRow, SQLError> {
    let schema = context.relations.view_schema(view)?;
    let columns = schema
        .columns()
        .iter()
        .enumerate()
        .map(|(position, column)| schema.public_name(position).unwrap_or(column).to_string())
        .collect::<Vec<_>>();
    let types = schema.column_types().to_vec();
    let values = columns
        .iter()
        .map(|column| document.get(column).cloned().unwrap_or(Value::Null))
        .collect();
    Ok(OwnedPhysicalRow::new(
        RowSchema::with_qualified_types(qualifier, columns, types),
        PhysicalRow::from_values(values),
    ))
}

pub mod commands;
