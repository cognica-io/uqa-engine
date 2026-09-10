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
use uqa_sql::{
    semantics::{
        collect_subquery_ids,
        view_rewrite::{self as logical, context::ViewRewriteContext},
    },
    RowSchema, SQLError, ScalarExpr,
};
use uqa_storage::document_store::Document;

#[derive(Clone)]
pub struct ViewRowContext<'a, S: Clone + 'static> {
    pub rewrite: ViewRewriteContext<'a>,
    pub rows: MutationRowContext<'a>,
    pub expressions: MutationExpressionContext<'a, S>,
}
impl<S: Clone + 'static> Copy for ViewRowContext<'_, S> {}

fn collect_expression_subquery_ids<'a>(
    expressions: impl IntoIterator<Item = &'a uqa_sql::ScalarExpr>,
) -> BTreeSet<usize> {
    let mut ids = BTreeSet::new();
    for expression in expressions {
        collect_subquery_ids(expression, &mut ids);
    }
    ids
}

struct AutomaticViewRuleProjection<'a, S: Clone + 'static> {
    context: ViewRowContext<'a, S>,
    document_relation: Option<&'a str>,
    storage_table: Option<&'a str>,
    doc_id: Option<DocId>,
    document: &'a Document,
    params: &'a [uqa_sql::SQLParam],
    scope: &'a CteScope<S>,
}

fn automatic_view_rule_document_inner<S: Clone + 'static>(
    projection: &AutomaticViewRuleProjection<'_, S>,
    view: &str,
    required_columns: &BTreeSet<String>,
    visited: &mut BTreeSet<String>,
) -> Result<Document, SQLError> {
    let layer =
        logical::automatic_view_layer(projection.context.rewrite, view)?.ok_or_else(|| {
            SQLError::Internal(format!(
                "rewrite-rule view `{view}` is not an automatic view layer"
            ))
        })?;
    if !visited.insert(layer.canonical_name.clone()) {
        return Err(SQLError::Internal(format!(
            "cycle while projecting rewrite-rule row for `{}`",
            layer.canonical_name
        )));
    }
    let selected_columns = layer
        .columns
        .iter()
        .filter(|column| required_columns.contains(&column.name))
        .collect::<Vec<_>>();
    let mut source_dependencies = BTreeSet::new();
    for column in &selected_columns {
        let mut expression = column.expression.clone();
        if !collect_expression_subquery_ids(std::iter::once(&expression)).is_empty() {
            source_dependencies.extend(logical::relation_columns(
                projection.context.rewrite,
                &layer.source_name,
            )?);
        }
        uqa_sql::plan::rewrite_scalar_expression(&mut expression, &mut |node| match node {
            ScalarExpr::Column(column) => {
                source_dependencies.insert(layer.physical_source_column(column).to_string());
            }
            ScalarExpr::QualifiedColumn { qualifier, column }
                if qualifier.eq_ignore_ascii_case(&layer.source_qualifier) =>
            {
                source_dependencies.insert(layer.physical_source_column(column).to_string());
            }
            _ => {}
        });
    }
    let source_document = if projection
        .document_relation
        .is_some_and(|relation| relation == layer.source_name)
    {
        Some(projection.document.clone())
    } else if projection
        .context
        .rewrite
        .catalog
        .view_definition(&layer.source_name)?
        .is_some()
    {
        Some(automatic_view_rule_document_inner(
            projection,
            &layer.source_name,
            &source_dependencies,
            visited,
        )?)
    } else {
        None
    };
    let source_row = if let Some(source_document) = source_document.as_ref() {
        stored_view_document_row(
            projection.context.rows,
            &layer.source_name,
            &layer.source_qualifier,
            source_document,
        )?
    } else {
        super::rows::target_row_for_storage_optional(
            projection.context.rows,
            &layer.source_name,
            projection.storage_table,
            &layer.source_qualifier,
            projection.doc_id,
            projection.document,
            Some(&source_dependencies),
        )?
    };
    let mut projected = Document::new();
    let mut layer_scope = projection.scope.clone();
    let layer_scope = layer_scope.enter_scalar_subqueries(&layer.subqueries);
    for column in selected_columns {
        let value = super::expressions::eval_mutation_expr(
            projection.context.expressions,
            &layer_scope,
            &column.expression,
            Some(&source_row),
            projection.params,
        )?;
        projected.insert(column.name.clone(), value);
    }
    visited.remove(&layer.canonical_name);
    Ok(projected)
}

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
    let projection = AutomaticViewRuleProjection {
        context: request.context,
        document_relation: request.document_relation,
        storage_table: request.storage_table,
        doc_id: request.doc_id,
        document: request.document,
        params: request.params,
        scope: request.scope,
    };
    automatic_view_rule_document_inner(
        &projection,
        request.view,
        request.required_columns,
        &mut BTreeSet::new(),
    )
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
