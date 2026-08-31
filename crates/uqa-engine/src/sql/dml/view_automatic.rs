//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` 18 automatically updatable view analysis and DML rewriting.

use std::collections::BTreeSet;

use uqa_core::DocId;
use uqa_execution::{ColumnIdentity, OwnedPhysicalRow, PhysicalRow, RowSchema, ScalarExpr};
use uqa_planner::{
    AssignmentPlan, ComputePlan, ConflictActionPlan, ConflictPlan, DeletePlan, InsertPlan,
    MergePlan, MergeWhenPlan, ProjectionPlan, QueryPlan, RelationalPlan, SourcePlan, UpdatePlan,
    ViewCheckPlan, ViewRuleInsertPlan, ViewRuleReturningPlan, ViewRuleUpdatePlan,
};
use uqa_sql::ast::{ReturningAliases, TriggerEvent, TriggerTiming};
use uqa_sql::SQLError;
use uqa_storage::document_store::Document;

use crate::{Engine, RelationIdentity, StoredView, StoredViewKind, Value};

use super::CteScope;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ViewCheckOption {
    None,
    Local,
    Cascaded,
}

impl ViewCheckOption {
    fn from_options(options: &[(String, String)]) -> Self {
        options
            .iter()
            .rev()
            .find(|(name, _)| name == "check_option")
            .map_or(Self::None, |(_, value)| match value.as_str() {
                "local" => Self::Local,
                "cascaded" => Self::Cascaded,
                _ => Self::None,
            })
    }

    const fn catalog_value(self) -> &'static str {
        match self {
            Self::None => "NONE",
            Self::Local => "LOCAL",
            Self::Cascaded => "CASCADED",
        }
    }
}

#[derive(Debug, Clone)]
struct ViewColumn {
    name: String,
    expression: ScalarExpr,
    writable_source_column: Option<String>,
}

#[derive(Debug, Clone)]
struct AutomaticViewLayer {
    canonical_name: String,
    source_name: String,
    source_qualifier: String,
    source_include_descendants: bool,
    source_schema: RowSchema,
    columns: Vec<ViewColumn>,
    predicate: Option<ScalarExpr>,
    subqueries: Vec<QueryPlan>,
    check_option: ViewCheckOption,
}

#[derive(Debug, Clone, Copy, Default)]
pub(in crate::sql) struct ViewMutationCapabilities {
    pub(in crate::sql) insertable: bool,
    pub(in crate::sql) updatable: bool,
    pub(in crate::sql) deletable: bool,
}

impl ViewMutationCapabilities {
    pub(in crate::sql) const fn fully_updatable(self) -> bool {
        self.updatable && self.deletable
    }
}

#[derive(Debug, Clone)]
pub(in crate::sql) struct ViewUpdatability {
    pub(in crate::sql) automatic: ViewMutationCapabilities,
    runtime: ViewMutationCapabilities,
    runtime_insert_columns: Vec<bool>,
    runtime_columns: Vec<bool>,
    pub(in crate::sql) catalog: ViewMutationCapabilities,
    catalog_insert_columns: Vec<bool>,
    pub(in crate::sql) catalog_columns: Vec<bool>,
    pub(in crate::sql) check_option: String,
}

fn display_relation(name: &str) -> String {
    RelationIdentity::from_legacy_name(name)
        .map_or_else(|_| name.to_string(), |relation| relation.name)
}

fn not_automatically_updatable(view: &str, operation: &str) -> SQLError {
    SQLError::Routine {
        sqlstate: "55000".into(),
        message: format!(
            "cannot {} view \"{}\": the view is not automatically updatable",
            operation.to_ascii_lowercase(),
            display_relation(view)
        ),
    }
}

fn non_writable_column(view: &str, column: &str, operation: &str) -> SQLError {
    SQLError::Routine {
        sqlstate: "0A000".into(),
        message: format!(
            "cannot {} column \"{column}\" of view \"{}\"",
            operation.to_ascii_lowercase(),
            display_relation(view)
        ),
    }
}

fn relation_columns(engine: &Engine, relation: &str) -> Result<Vec<String>, SQLError> {
    if let Some(view) = engine.view_definition(relation)? {
        let schema = engine.stored_view_schema(&view)?;
        return Ok(schema
            .columns()
            .iter()
            .enumerate()
            .map(|(position, column)| {
                schema
                    .public_name(position)
                    .map_or_else(|| column.clone(), str::to_string)
            })
            .collect());
    }
    let definitions = engine
        .try_describe_table(relation)
        .map_err(|error| SQLError::Internal(format!("describe view source `{relation}`: {error}")))?
        .ok_or_else(|| SQLError::UnknownTable(relation.to_string()))?;
    if definitions.is_empty() {
        return engine.try_table_columns(relation).map_err(|error| {
            SQLError::Internal(format!("describe view source `{relation}`: {error}"))
        });
    }
    Ok(definitions
        .into_iter()
        .map(|definition| definition.name)
        .collect())
}

fn source_qualifier_matches(qualifier: &str, source_qualifier: &str, source_name: &str) -> bool {
    qualifier == source_qualifier
        || qualifier == source_name
        || RelationIdentity::from_legacy_name(source_name)
            .is_ok_and(|identity| qualifier == identity.name)
}

fn direct_source_column(
    expression: &ScalarExpr,
    source_qualifier: &str,
    source_name: &str,
    source_columns: &BTreeSet<String>,
) -> Option<String> {
    let column = match expression {
        ScalarExpr::Column(column) => Some(column.clone()),
        ScalarExpr::QualifiedColumn { qualifier, column }
            if source_qualifier_matches(qualifier, source_qualifier, source_name) =>
        {
            Some(column.clone())
        }
        _ => None,
    }?;
    source_columns.contains(&column).then_some(column)
}

fn automatic_view_layer(
    engine: &Engine,
    name: &str,
) -> Result<Option<AutomaticViewLayer>, SQLError> {
    let Some(canonical_name) = engine
        .try_resolve_view_name(name)
        .map_err(|error| SQLError::Internal(format!("resolve DML view `{name}`: {error}")))?
    else {
        return Ok(None);
    };
    let definition = engine
        .view_definition(&canonical_name)?
        .ok_or_else(|| SQLError::UnknownTable(name.to_string()))?;
    automatic_view_layer_from_definition(engine, &canonical_name, &definition)
}

fn automatic_view_layer_from_definition(
    engine: &Engine,
    canonical_name: &str,
    definition: &StoredView,
) -> Result<Option<AutomaticViewLayer>, SQLError> {
    if definition.kind != StoredViewKind::View || !definition.query.ctes.is_empty() {
        return Ok(None);
    }
    let RelationalPlan::QueryBlock(block) = &definition.query.root else {
        return Ok(None);
    };
    if !matches!(block.compute, ComputePlan::Project)
        || !block.group_by.is_empty()
        || !block.grouping_sets.is_empty()
        || block.group_distinct
        || block.having.is_some()
        || block.limit.is_some()
        || block.with_ties
        || block.offset.is_some()
        || block.distinct
        || !block.distinct_on.is_empty()
        || !block.locking.is_empty()
    {
        return Ok(None);
    }
    let Some(
        source_plan @ SourcePlan::Table {
            name: source_name,
            qualifier,
            alias,
            include_descendants,
        },
    ) = block.from.as_ref()
    else {
        return Ok(None);
    };
    let analysis_scope = CteScope::new_for_current_routine();
    let source_schema = crate::sql::select::analyze_source_plan_schema(
        engine,
        source_plan,
        &[],
        &analysis_scope,
        None,
    )?;
    let resolver = crate::sql::select::ScopedEngineHook::new(engine, &analysis_scope);
    for projection in &block.projections {
        if crate::sql::select::expression_may_return_set(
            engine,
            &resolver,
            &projection.expr,
            &source_schema,
            &[],
        )? {
            return Ok(None);
        }
    }
    let source_qualifier = alias.as_deref().unwrap_or(qualifier).to_string();
    let source_columns = relation_columns(engine, source_name)?;
    let source_column_set = source_columns.iter().cloned().collect::<BTreeSet<_>>();
    let mut expressions = Vec::new();
    for projection in &block.projections {
        match &projection.expr {
            ScalarExpr::Star => {
                expressions.extend(source_columns.iter().cloned().map(ScalarExpr::Column));
            }
            ScalarExpr::QualifiedStar(star_qualifier)
                if source_qualifier_matches(star_qualifier, &source_qualifier, source_name) =>
            {
                expressions.extend(source_columns.iter().cloned().map(|column| {
                    ScalarExpr::QualifiedColumn {
                        qualifier: source_qualifier.clone(),
                        column,
                    }
                }));
            }
            expression => expressions.push(expression.clone()),
        }
    }
    let schema = engine.stored_view_schema(definition)?;
    let output_columns = schema
        .columns()
        .iter()
        .enumerate()
        .map(|(position, column)| schema.public_name(position).unwrap_or(column).to_string())
        .collect::<Vec<_>>();
    if expressions.len() != output_columns.len() {
        return Err(SQLError::Internal(format!(
            "view `{canonical_name}` has {} stored projections for {} output columns",
            expressions.len(),
            output_columns.len()
        )));
    }
    let columns = output_columns
        .into_iter()
        .zip(expressions)
        .map(|(name, expression)| ViewColumn {
            writable_source_column: direct_source_column(
                &expression,
                &source_qualifier,
                source_name,
                &source_column_set,
            ),
            name,
            expression,
        })
        .collect();
    Ok(Some(AutomaticViewLayer {
        canonical_name: canonical_name.to_string(),
        source_name: source_name.clone(),
        source_qualifier,
        source_include_descendants: *include_descendants,
        source_schema,
        columns,
        predicate: block.r#where.clone(),
        subqueries: block.subqueries.clone(),
        check_option: ViewCheckOption::from_options(&definition.options),
    }))
}

fn offset_expression_subquery_ids(expression: &mut ScalarExpr, offset: usize) {
    if offset == 0 {
        return;
    }
    uqa_planner::rewrite_scalar_expression(expression, &mut |node| match node {
        ScalarExpr::ScalarSubquery(id)
        | ScalarExpr::Exists { subquery: id, .. }
        | ScalarExpr::InSubquery { subquery: id, .. } => *id += offset,
        _ => {}
    });
}

fn rewrite_layer_source_scalar(
    expression: &mut ScalarExpr,
    layer: &AutomaticViewLayer,
    target_qualifier: &str,
    shadowed_qualifiers: &BTreeSet<String>,
    shadowed_columns: &BTreeSet<String>,
) {
    uqa_planner::rewrite_scalar_expression(expression, &mut |node| {
        let replacement = match node {
            ScalarExpr::Column(column)
                if !shadowed_columns.contains(column)
                    && layer.source_schema.has_unqualified_column(column) =>
            {
                Some(ScalarExpr::QualifiedColumn {
                    qualifier: target_qualifier.to_string(),
                    column: column.clone(),
                })
            }
            ScalarExpr::QualifiedColumn { qualifier, column }
                if !shadowed_qualifiers.contains(qualifier)
                    && source_qualifier_matches(
                        qualifier,
                        &layer.source_qualifier,
                        &layer.source_name,
                    ) =>
            {
                Some(ScalarExpr::QualifiedColumn {
                    qualifier: target_qualifier.to_string(),
                    column: column.clone(),
                })
            }
            _ => None,
        };
        if let Some(replacement) = replacement {
            *node = replacement;
        }
    });
}

fn source_plan_declares_qualifier(source: &SourcePlan, qualifier: &str) -> bool {
    match source {
        SourcePlan::Table {
            qualifier: source_qualifier,
            alias,
            ..
        } => alias.as_deref().unwrap_or(source_qualifier) == qualifier,
        SourcePlan::Join {
            left, right, alias, ..
        } => {
            alias.as_deref() == Some(qualifier)
                || source_plan_declares_qualifier(left, qualifier)
                || source_plan_declares_qualifier(right, qualifier)
        }
        SourcePlan::Values { alias, .. } | SourcePlan::Subquery { alias, .. } => {
            alias.as_deref() == Some(qualifier)
        }
        SourcePlan::Function {
            output_name, alias, ..
        } => alias.as_deref().unwrap_or(output_name) == qualifier,
        SourcePlan::FunctionGroup {
            functions, alias, ..
        } => {
            alias.as_deref().or_else(|| {
                functions
                    .first()
                    .map(|function| function.output_name.as_str())
            }) == Some(qualifier)
        }
    }
}

fn rename_source_plan_qualifier(source: &mut SourcePlan, qualifier: &str, replacement: &str) {
    match source {
        SourcePlan::Table {
            qualifier: source_qualifier,
            alias,
            ..
        } => {
            if alias.as_deref() == Some(qualifier) {
                *alias = Some(replacement.to_string());
            } else if alias.is_none() && source_qualifier == qualifier {
                *source_qualifier = replacement.to_string();
            }
        }
        SourcePlan::Join {
            left, right, alias, ..
        } => {
            rename_source_plan_qualifier(left, qualifier, replacement);
            rename_source_plan_qualifier(right, qualifier, replacement);
            if alias.as_deref() == Some(qualifier) {
                *alias = Some(replacement.to_string());
            }
        }
        SourcePlan::Values { alias, .. } | SourcePlan::Subquery { alias, .. } => {
            if alias.as_deref() == Some(qualifier) {
                *alias = Some(replacement.to_string());
            }
        }
        SourcePlan::Function {
            output_name, alias, ..
        } => {
            if alias.as_deref() == Some(qualifier) || alias.is_none() && output_name == qualifier {
                *alias = Some(replacement.to_string());
            }
        }
        SourcePlan::FunctionGroup {
            functions, alias, ..
        } => {
            let default_matches = alias.is_none()
                && functions
                    .first()
                    .is_some_and(|function| function.output_name == qualifier);
            if alias.as_deref() == Some(qualifier) || default_matches {
                *alias = Some(replacement.to_string());
            }
        }
    }
}

fn rename_qualified_scalar(expression: &mut ScalarExpr, qualifier: &str, replacement: &str) {
    uqa_planner::rewrite_scalar_expression(expression, &mut |node| match node {
        ScalarExpr::QualifiedColumn {
            qualifier: current, ..
        }
        | ScalarExpr::QualifiedStar(current)
            if current == qualifier =>
        {
            *current = replacement.to_string();
        }
        _ => {}
    });
}

fn rename_source_plan_scalar_qualifiers(
    source: &mut SourcePlan,
    qualifier: &str,
    replacement: &str,
) {
    match source {
        SourcePlan::Table { .. } | SourcePlan::Subquery { .. } => {}
        SourcePlan::Join {
            left, right, on, ..
        } => {
            rename_source_plan_scalar_qualifiers(left, qualifier, replacement);
            rename_source_plan_scalar_qualifiers(right, qualifier, replacement);
            if let Some(on) = on {
                rename_qualified_scalar(on, qualifier, replacement);
            }
        }
        SourcePlan::Values { rows, .. } => {
            for expression in rows.iter_mut().flatten() {
                rename_qualified_scalar(expression, qualifier, replacement);
            }
        }
        SourcePlan::Function { args, .. } => {
            for expression in args {
                rename_qualified_scalar(expression, qualifier, replacement);
            }
        }
        SourcePlan::FunctionGroup { functions, .. } => {
            for expression in functions
                .iter_mut()
                .flat_map(|function| function.args.iter_mut())
            {
                rename_qualified_scalar(expression, qualifier, replacement);
            }
        }
    }
}

fn rename_source_plan_subqueries(
    source: &mut SourcePlan,
    qualifier: &str,
    replacement: &str,
    inherited: bool,
) {
    match source {
        SourcePlan::Join { left, right, .. } => {
            rename_source_plan_subqueries(left, qualifier, replacement, inherited);
            rename_source_plan_subqueries(right, qualifier, replacement, inherited);
        }
        SourcePlan::Subquery { body, .. } => {
            rename_shadowing_query_qualifier(body, qualifier, replacement, inherited);
        }
        SourcePlan::Table { .. }
        | SourcePlan::Values { .. }
        | SourcePlan::Function { .. }
        | SourcePlan::FunctionGroup { .. } => {}
    }
}

fn rename_shadowing_query_qualifier(
    query: &mut QueryPlan,
    qualifier: &str,
    replacement: &str,
    inherited: bool,
) {
    for cte in &mut query.ctes {
        rename_shadowing_query_qualifier(&mut cte.query, qualifier, replacement, inherited);
    }
    match &mut query.root {
        RelationalPlan::QueryBlock(block) => {
            if let Some(source) = &mut block.from {
                rename_source_plan_subqueries(source, qualifier, replacement, inherited);
            }
            let declared = block
                .from
                .as_ref()
                .is_some_and(|source| source_plan_declares_qualifier(source, qualifier));
            let active = inherited || declared;
            if declared {
                rename_source_plan_qualifier(
                    block.from.as_mut().expect("view subquery source exists"),
                    qualifier,
                    replacement,
                );
            }
            if active {
                if let Some(source) = &mut block.from {
                    rename_source_plan_scalar_qualifiers(source, qualifier, replacement);
                }
                for projection in &mut block.projections {
                    rename_qualified_scalar(&mut projection.expr, qualifier, replacement);
                }
                for expression in block
                    .r#where
                    .iter_mut()
                    .chain(block.group_by.iter_mut())
                    .chain(block.having.iter_mut())
                    .chain(block.limit.iter_mut())
                    .chain(block.offset.iter_mut())
                    .chain(block.distinct_on.iter_mut())
                {
                    rename_qualified_scalar(expression, qualifier, replacement);
                }
                for set in &mut block.grouping_sets {
                    for expression in set {
                        rename_qualified_scalar(expression, qualifier, replacement);
                    }
                }
                for order in &mut block.order_by {
                    rename_qualified_scalar(&mut order.expr, qualifier, replacement);
                }
            }
            for subquery in &mut block.subqueries {
                rename_shadowing_query_qualifier(subquery, qualifier, replacement, active);
            }
        }
        RelationalPlan::SetOp {
            left,
            right,
            order_by,
            limit,
            offset,
            subqueries,
            ..
        } => {
            rename_shadowing_query_qualifier(left, qualifier, replacement, inherited);
            rename_shadowing_query_qualifier(right, qualifier, replacement, inherited);
            if inherited {
                for order in order_by {
                    rename_qualified_scalar(&mut order.expr, qualifier, replacement);
                }
                for expression in [limit.as_deref_mut(), offset.as_deref_mut()]
                    .into_iter()
                    .flatten()
                {
                    rename_qualified_scalar(expression, qualifier, replacement);
                }
            }
            for subquery in subqueries {
                rename_shadowing_query_qualifier(subquery, qualifier, replacement, inherited);
            }
        }
        RelationalPlan::Values { rows, subqueries } => {
            if inherited {
                for expression in rows.iter_mut().flatten() {
                    rename_qualified_scalar(expression, qualifier, replacement);
                }
            }
            for subquery in subqueries {
                rename_shadowing_query_qualifier(subquery, qualifier, replacement, inherited);
            }
        }
    }
}

struct LayerSubqueryRewriteContext<'a> {
    engine: &'a Engine,
    layer: &'a AutomaticViewLayer,
    target_qualifier: &'a str,
}

fn rewrite_layer_source_plan(
    context: &LayerSubqueryRewriteContext<'_>,
    source: &mut SourcePlan,
    scope: &CteScope,
    shadowed_qualifiers: &BTreeSet<String>,
    shadowed_columns: &BTreeSet<String>,
) -> Result<(), SQLError> {
    let rewrite = |expression: &mut ScalarExpr| {
        rewrite_layer_source_scalar(
            expression,
            context.layer,
            context.target_qualifier,
            shadowed_qualifiers,
            shadowed_columns,
        );
    };
    match source {
        SourcePlan::Table { .. } => {}
        SourcePlan::Join {
            left, right, on, ..
        } => {
            rewrite_layer_source_plan(context, left, scope, shadowed_qualifiers, shadowed_columns)?;
            rewrite_layer_source_plan(
                context,
                right,
                scope,
                shadowed_qualifiers,
                shadowed_columns,
            )?;
            if let Some(on) = on {
                rewrite(on);
            }
        }
        SourcePlan::Values { rows, .. } => {
            for expression in rows.iter_mut().flatten() {
                rewrite(expression);
            }
        }
        SourcePlan::Function { args, .. } => {
            for expression in args {
                rewrite(expression);
            }
        }
        SourcePlan::FunctionGroup { functions, .. } => {
            for expression in functions
                .iter_mut()
                .flat_map(|function| function.args.iter_mut())
            {
                rewrite(expression);
            }
        }
        SourcePlan::Subquery { body, .. } => {
            rewrite_layer_source_query(
                context,
                body,
                scope,
                shadowed_qualifiers,
                shadowed_columns,
            )?;
        }
    }
    Ok(())
}

fn rewrite_layer_source_query(
    context: &LayerSubqueryRewriteContext<'_>,
    query: &mut QueryPlan,
    scope: &CteScope,
    inherited_qualifier_shadows: &BTreeSet<String>,
    inherited_column_shadows: &BTreeSet<String>,
) -> Result<(), SQLError> {
    let mut query_scope = scope.clone();
    for cte in &query.ctes {
        query_scope.insert_deferred(cte.clone());
    }
    for cte in &mut query.ctes {
        rewrite_layer_source_query(
            context,
            &mut cte.query,
            &query_scope,
            inherited_qualifier_shadows,
            inherited_column_shadows,
        )?;
    }
    match &mut query.root {
        RelationalPlan::QueryBlock(block) => {
            let local_schema = block
                .from
                .as_ref()
                .map(|source| {
                    crate::sql::select::analyze_source_plan_schema(
                        context.engine,
                        source,
                        &[],
                        &query_scope,
                        Some(&context.layer.source_schema),
                    )
                })
                .transpose()?;
            let mut qualifier_shadows = inherited_qualifier_shadows.clone();
            if let Some(schema) = local_schema.as_ref() {
                for qualifier in [
                    context.layer.source_qualifier.as_str(),
                    context.layer.source_name.as_str(),
                    display_relation(&context.layer.source_name).as_str(),
                ] {
                    if schema.has_qualifier(qualifier) {
                        qualifier_shadows.insert(qualifier.to_string());
                    }
                }
            }
            let mut column_shadows = inherited_column_shadows.clone();
            if let Some(schema) = local_schema.as_ref() {
                column_shadows.extend(schema_public_columns(schema));
            }
            if let Some(source) = &mut block.from {
                rewrite_layer_source_plan(
                    context,
                    source,
                    &query_scope,
                    &qualifier_shadows,
                    &column_shadows,
                )?;
            }
            let rewrite = |expression: &mut ScalarExpr| {
                rewrite_layer_source_scalar(
                    expression,
                    context.layer,
                    context.target_qualifier,
                    &qualifier_shadows,
                    &column_shadows,
                );
            };
            for projection in &mut block.projections {
                rewrite(&mut projection.expr);
            }
            if let Some(predicate) = &mut block.r#where {
                rewrite(predicate);
            }
            for expression in &mut block.group_by {
                rewrite(expression);
            }
            for set in &mut block.grouping_sets {
                for expression in set {
                    rewrite(expression);
                }
            }
            if let Some(having) = &mut block.having {
                rewrite(having);
            }
            for order in &mut block.order_by {
                rewrite(&mut order.expr);
            }
            for expression in [block.limit.as_mut(), block.offset.as_mut()]
                .into_iter()
                .flatten()
            {
                rewrite(expression);
            }
            for expression in &mut block.distinct_on {
                rewrite(expression);
            }
            for subquery in &mut block.subqueries {
                rewrite_layer_source_query(
                    context,
                    subquery,
                    &query_scope,
                    &qualifier_shadows,
                    &column_shadows,
                )?;
            }
        }
        RelationalPlan::SetOp {
            left,
            right,
            order_by,
            limit,
            offset,
            subqueries,
            ..
        } => {
            rewrite_layer_source_query(
                context,
                left,
                &query_scope,
                inherited_qualifier_shadows,
                inherited_column_shadows,
            )?;
            rewrite_layer_source_query(
                context,
                right,
                &query_scope,
                inherited_qualifier_shadows,
                inherited_column_shadows,
            )?;
            for order in order_by {
                rewrite_layer_source_scalar(
                    &mut order.expr,
                    context.layer,
                    context.target_qualifier,
                    inherited_qualifier_shadows,
                    inherited_column_shadows,
                );
            }
            for expression in [limit.as_deref_mut(), offset.as_deref_mut()]
                .into_iter()
                .flatten()
            {
                rewrite_layer_source_scalar(
                    expression,
                    context.layer,
                    context.target_qualifier,
                    inherited_qualifier_shadows,
                    inherited_column_shadows,
                );
            }
            for subquery in subqueries {
                rewrite_layer_source_query(
                    context,
                    subquery,
                    &query_scope,
                    inherited_qualifier_shadows,
                    inherited_column_shadows,
                )?;
            }
        }
        RelationalPlan::Values { rows, subqueries } => {
            for expression in rows.iter_mut().flatten() {
                rewrite_layer_source_scalar(
                    expression,
                    context.layer,
                    context.target_qualifier,
                    inherited_qualifier_shadows,
                    inherited_column_shadows,
                );
            }
            for subquery in subqueries {
                rewrite_layer_source_query(
                    context,
                    subquery,
                    &query_scope,
                    inherited_qualifier_shadows,
                    inherited_column_shadows,
                )?;
            }
        }
    }
    Ok(())
}

fn embed_layer_expression(
    engine: &Engine,
    expression: &ScalarExpr,
    layer: &AutomaticViewLayer,
    target_qualifier: &str,
    target_subqueries: &mut Vec<QueryPlan>,
) -> Result<ScalarExpr, SQLError> {
    let mut expression = retarget_source_expression(expression, layer, target_qualifier);
    let ids = collect_expression_subquery_ids(std::iter::once(&expression));
    if ids.is_empty() {
        return Ok(expression);
    }
    if ids
        .iter()
        .next_back()
        .is_some_and(|id| *id >= layer.subqueries.len())
    {
        return Err(SQLError::Internal(format!(
            "view `{}` expression has an out-of-bounds scalar subquery slot",
            layer.canonical_name
        )));
    }
    let mut subqueries = layer.subqueries.clone();
    let scope = CteScope::new_for_current_routine();
    let context = LayerSubqueryRewriteContext {
        engine,
        layer,
        target_qualifier,
    };
    for subquery in &mut subqueries {
        rename_shadowing_query_qualifier(
            subquery,
            target_qualifier,
            "\0uqa_view_subquery_local",
            false,
        );
        rewrite_layer_source_query(
            &context,
            subquery,
            &scope,
            &BTreeSet::new(),
            &BTreeSet::new(),
        )?;
    }
    let offset = target_subqueries.len();
    offset_expression_subquery_ids(&mut expression, offset);
    target_subqueries.extend(subqueries);
    Ok(expression)
}

fn layer_column<'a>(layer: &'a AutomaticViewLayer, name: &str) -> Option<&'a ViewColumn> {
    layer.columns.iter().find(|column| column.name == name)
}

fn unknown_view_column(layer: &AutomaticViewLayer, column: &str) -> SQLError {
    SQLError::UnknownColumn(format!("{}.{column}", layer.canonical_name))
}

fn duplicate_insert_column(column: &str) -> SQLError {
    SQLError::Routine {
        sqlstate: "42701".into(),
        message: format!("column \"{column}\" specified more than once"),
    }
}

fn duplicate_assignment(column: &str) -> SQLError {
    SQLError::Routine {
        sqlstate: "42601".into(),
        message: format!("multiple assignments to same column \"{column}\""),
    }
}

fn validate_view_target_columns<'a>(
    layer: &AutomaticViewLayer,
    columns: impl IntoIterator<Item = &'a str>,
    duplicate: fn(&str) -> SQLError,
) -> Result<(), SQLError> {
    let mut seen = BTreeSet::new();
    for column in columns {
        if layer_column(layer, column).is_none() {
            return Err(unknown_view_column(layer, column));
        }
        if !seen.insert(column) {
            return Err(duplicate(column));
        }
    }
    Ok(())
}

fn validate_mapped_columns(
    columns: &[String],
    duplicate: fn(&str) -> SQLError,
) -> Result<(), SQLError> {
    let mut seen = BTreeSet::new();
    for column in columns {
        if !seen.insert(column) {
            return Err(duplicate(column));
        }
    }
    Ok(())
}

fn validate_insert_targets(layer: &AutomaticViewLayer, plan: &InsertPlan) -> Result<(), SQLError> {
    validate_view_target_columns(
        layer,
        plan.columns.iter().map(String::as_str),
        duplicate_insert_column,
    )?;
    let Some(conflict) = &plan.on_conflict else {
        return Ok(());
    };
    for column in &conflict.conflict_columns {
        if layer_column(layer, column).is_none() {
            return Err(unknown_view_column(layer, column));
        }
    }
    if let ConflictActionPlan::Update { assignments, .. } = &conflict.action {
        validate_view_target_columns(
            layer,
            assignments
                .iter()
                .map(|assignment| assignment.column.as_str()),
            duplicate_assignment,
        )?;
    }
    Ok(())
}

fn validate_update_targets(layer: &AutomaticViewLayer, plan: &UpdatePlan) -> Result<(), SQLError> {
    validate_view_target_columns(
        layer,
        plan.assignments
            .iter()
            .map(|assignment| assignment.column.as_str()),
        duplicate_assignment,
    )
}

fn writable_column(
    layer: &AutomaticViewLayer,
    name: &str,
    operation: &str,
) -> Result<String, SQLError> {
    let column = layer_column(layer, name)
        .ok_or_else(|| SQLError::UnknownColumn(format!("{}.{name}", layer.canonical_name)))?;
    column
        .writable_source_column
        .clone()
        .ok_or_else(|| non_writable_column(&layer.canonical_name, name, operation))
}

fn ambiguous_column(column: &str) -> SQLError {
    SQLError::Routine {
        sqlstate: "42702".into(),
        message: format!("column reference \"{column}\" is ambiguous"),
    }
}

#[derive(Clone, Copy)]
struct ExpressionScope<'a> {
    target_qualifier: &'a str,
    returning_aliases: Option<&'a ReturningAliases>,
    source: Option<&'a RowSchema>,
    include_excluded: bool,
}

impl ExpressionScope<'_> {
    fn row_image_qualifier(self, qualifier: &str) -> bool {
        self.returning_aliases.is_some_and(|aliases| {
            [
                (aliases.old.as_str(), aliases.old_explicit),
                (aliases.new.as_str(), aliases.new_explicit),
            ]
            .into_iter()
            .any(|(alias, explicit)| {
                qualifier == alias
                    && (explicit
                        || !self
                            .source
                            .is_some_and(|source| source.has_qualifier(alias)))
            })
        })
    }

    fn target_qualifier(self, qualifier: &str) -> bool {
        qualifier == self.target_qualifier
            || (self.include_excluded && qualifier == "excluded")
            || self.row_image_qualifier(qualifier)
    }
}

fn validate_view_expression(
    expression: &ScalarExpr,
    layer: &AutomaticViewLayer,
    scope: ExpressionScope<'_>,
) -> Result<(), SQLError> {
    let mut expression = expression.clone();
    let mut error = None;
    uqa_planner::rewrite_scalar_expression(&mut expression, &mut |node| {
        if error.is_some() {
            return;
        }
        match node {
            ScalarExpr::Column(column) => {
                let target = layer_column(layer, column).is_some();
                let source = scope
                    .source
                    .is_some_and(|source| source.has_unqualified_column(column));
                if target && (source || scope.include_excluded) {
                    error = Some(ambiguous_column(column));
                } else if !target && !source {
                    error = Some(SQLError::UnknownColumn(column.clone()));
                }
            }
            ScalarExpr::QualifiedColumn { qualifier, column }
                if scope.target_qualifier(qualifier) && layer_column(layer, column).is_none() =>
            {
                error = Some(SQLError::UnknownColumn(format!("{qualifier}.{column}")));
            }
            _ => {}
        }
    });
    error.map_or(Ok(()), Err)
}

fn public_view_columns(engine: &Engine, view: &str) -> Result<BTreeSet<String>, SQLError> {
    let definition = engine
        .view_definition(view)?
        .ok_or_else(|| SQLError::UnknownTable(view.to_string()))?;
    let schema = engine.stored_view_schema(&definition)?;
    Ok(schema
        .columns()
        .iter()
        .enumerate()
        .map(|(position, column)| schema.public_name(position).unwrap_or(column).to_string())
        .collect())
}

fn validate_public_view_expression(
    expression: &ScalarExpr,
    public_columns: &BTreeSet<String>,
    scope: ExpressionScope<'_>,
) -> Result<(), SQLError> {
    let mut expression = expression.clone();
    let mut error = None;
    uqa_planner::rewrite_scalar_expression(&mut expression, &mut |node| {
        if error.is_some() {
            return;
        }
        match node {
            ScalarExpr::Column(column) => {
                let target = public_columns.contains(column);
                let source = scope
                    .source
                    .is_some_and(|source| source.has_unqualified_column(column));
                if target && (source || scope.include_excluded) {
                    error = Some(ambiguous_column(column));
                } else if !target && !source {
                    error = Some(SQLError::UnknownColumn(column.clone()));
                }
            }
            ScalarExpr::QualifiedColumn { qualifier, column }
                if scope.target_qualifier(qualifier) && !public_columns.contains(column) =>
            {
                error = Some(SQLError::UnknownColumn(format!("{qualifier}.{column}")));
            }
            _ => {}
        }
    });
    error.map_or(Ok(()), Err)
}

fn validate_public_update_contract(
    engine: &Engine,
    plan: &UpdatePlan,
    source: Option<&RowSchema>,
) -> Result<(), SQLError> {
    let columns = public_view_columns(engine, &plan.table)?;
    let ordinary_scope = ExpressionScope {
        target_qualifier: &plan.target_qualifier,
        returning_aliases: None,
        source,
        include_excluded: false,
    };
    for assignment in &plan.assignments {
        validate_public_view_expression(&assignment.value, &columns, ordinary_scope)?;
    }
    if let Some(predicate) = plan.predicate.as_ref() {
        validate_public_view_expression(predicate, &columns, ordinary_scope)?;
    }
    let returning_scope = ExpressionScope {
        returning_aliases: Some(&plan.returning_aliases),
        ..ordinary_scope
    };
    for projection in &plan.returning {
        validate_public_view_expression(&projection.expr, &columns, returning_scope)?;
    }
    Ok(())
}

fn validate_public_delete_contract(
    engine: &Engine,
    plan: &DeletePlan,
    source: Option<&RowSchema>,
) -> Result<(), SQLError> {
    let columns = public_view_columns(engine, &plan.table)?;
    let ordinary_scope = ExpressionScope {
        target_qualifier: &plan.target_qualifier,
        returning_aliases: None,
        source,
        include_excluded: false,
    };
    if let Some(predicate) = plan.predicate.as_ref() {
        validate_public_view_expression(predicate, &columns, ordinary_scope)?;
    }
    let returning_scope = ExpressionScope {
        returning_aliases: Some(&plan.returning_aliases),
        ..ordinary_scope
    };
    for projection in &plan.returning {
        validate_public_view_expression(&projection.expr, &columns, returning_scope)?;
    }
    Ok(())
}

fn validate_public_insert_contract(engine: &Engine, plan: &InsertPlan) -> Result<(), SQLError> {
    let columns = public_view_columns(engine, &plan.table)?;
    if let Some(ConflictPlan {
        action:
            ConflictActionPlan::Update {
                assignments,
                predicate,
            },
        ..
    }) = &plan.on_conflict
    {
        let scope = ExpressionScope {
            target_qualifier: &plan.target_qualifier,
            returning_aliases: None,
            source: None,
            include_excluded: true,
        };
        for assignment in assignments {
            validate_public_view_expression(&assignment.value, &columns, scope)?;
        }
        if let Some(predicate) = predicate {
            validate_public_view_expression(predicate, &columns, scope)?;
        }
    }
    let scope = ExpressionScope {
        target_qualifier: &plan.target_qualifier,
        returning_aliases: Some(&plan.returning_aliases),
        source: None,
        include_excluded: false,
    };
    for projection in &plan.returning {
        validate_public_view_expression(&projection.expr, &columns, scope)?;
    }
    Ok(())
}

pub(in crate::sql) fn validate_public_merge_contract(
    engine: &Engine,
    plan: &MergePlan,
    source: &RowSchema,
) -> Result<(), SQLError> {
    let columns = public_view_columns(engine, &plan.target)?;
    let matched_scope = ExpressionScope {
        target_qualifier: &plan.target_qualifier,
        returning_aliases: None,
        source: Some(source),
        include_excluded: false,
    };
    let target_only_scope = ExpressionScope {
        source: None,
        ..matched_scope
    };
    validate_public_view_expression(&plan.join_condition, &columns, matched_scope)?;
    for clause in &plan.when_clauses {
        match clause {
            MergeWhenPlan::UpdateMatched {
                condition,
                assignments,
            } => {
                if let Some(condition) = condition {
                    validate_public_view_expression(condition, &columns, matched_scope)?;
                }
                for assignment in assignments {
                    validate_public_view_expression(&assignment.value, &columns, matched_scope)?;
                }
            }
            MergeWhenPlan::DeleteMatched { condition }
            | MergeWhenPlan::NothingMatched { condition } => {
                if let Some(condition) = condition {
                    validate_public_view_expression(condition, &columns, matched_scope)?;
                }
            }
            MergeWhenPlan::UpdateNotMatchedBySource {
                condition,
                assignments,
            } => {
                if let Some(condition) = condition {
                    validate_public_view_expression(condition, &columns, target_only_scope)?;
                }
                for assignment in assignments {
                    validate_public_view_expression(
                        &assignment.value,
                        &columns,
                        target_only_scope,
                    )?;
                }
            }
            MergeWhenPlan::DeleteNotMatchedBySource { condition }
            | MergeWhenPlan::NothingNotMatchedBySource { condition } => {
                if let Some(condition) = condition {
                    validate_public_view_expression(condition, &columns, target_only_scope)?;
                }
            }
            MergeWhenPlan::InsertNotMatched { .. } | MergeWhenPlan::NothingNotMatched { .. } => {}
        }
    }
    let returning_scope = ExpressionScope {
        returning_aliases: Some(&plan.returning_aliases),
        ..matched_scope
    };
    for projection in &plan.returning {
        validate_public_view_expression(&projection.expr, &columns, returning_scope)?;
    }
    Ok(())
}

fn validate_merge_targets(layer: &AutomaticViewLayer, plan: &MergePlan) -> Result<(), SQLError> {
    for clause in &plan.when_clauses {
        match clause {
            MergeWhenPlan::UpdateMatched { assignments, .. }
            | MergeWhenPlan::UpdateNotMatchedBySource { assignments, .. } => {
                validate_view_target_columns(
                    layer,
                    assignments
                        .iter()
                        .map(|assignment| assignment.column.as_str()),
                    duplicate_assignment,
                )?;
            }
            MergeWhenPlan::InsertNotMatched { columns, .. } if !columns.is_empty() => {
                validate_view_target_columns(
                    layer,
                    columns.iter().map(String::as_str),
                    duplicate_insert_column,
                )?;
            }
            _ => {}
        }
    }
    Ok(())
}

pub(in crate::sql) fn validate_public_merge_targets(
    engine: &Engine,
    plan: &MergePlan,
) -> Result<(), SQLError> {
    for clause in &plan.when_clauses {
        match clause {
            MergeWhenPlan::UpdateMatched { assignments, .. }
            | MergeWhenPlan::UpdateNotMatchedBySource { assignments, .. } => {
                let columns = assignments
                    .iter()
                    .map(|assignment| assignment.column.as_str())
                    .collect::<Vec<_>>();
                validate_public_view_targets(engine, &plan.target, columns.iter().copied())?;
                validate_mapped_columns(
                    &columns
                        .iter()
                        .map(|column| (*column).to_string())
                        .collect::<Vec<_>>(),
                    duplicate_assignment,
                )?;
            }
            MergeWhenPlan::InsertNotMatched { columns, .. } if !columns.is_empty() => {
                validate_public_view_targets(
                    engine,
                    &plan.target,
                    columns.iter().map(String::as_str),
                )?;
                validate_mapped_columns(columns, duplicate_insert_column)?;
            }
            _ => {}
        }
    }
    Ok(())
}

fn merge_action_capability_error(
    view: &str,
    clauses: &[MergeWhenPlan],
    capabilities: ViewMutationCapabilities,
) -> Option<SQLError> {
    clauses.iter().find_map(|clause| match clause {
        MergeWhenPlan::UpdateMatched { .. } | MergeWhenPlan::UpdateNotMatchedBySource { .. }
            if !capabilities.updatable =>
        {
            Some(not_automatically_updatable(view, "UPDATE"))
        }
        MergeWhenPlan::DeleteMatched { .. } | MergeWhenPlan::DeleteNotMatchedBySource { .. }
            if !capabilities.deletable =>
        {
            Some(not_automatically_updatable(view, "DELETE FROM"))
        }
        MergeWhenPlan::InsertNotMatched { .. } if !capabilities.insertable => {
            Some(not_automatically_updatable(view, "INSERT INTO"))
        }
        _ => None,
    })
}

fn validate_merge_rule_free(engine: &Engine, relation: &str) -> Result<(), SQLError> {
    let has_rules = [
        uqa_sql::ast::RuleEvent::Insert,
        uqa_sql::ast::RuleEvent::Update,
        uqa_sql::ast::RuleEvent::Delete,
    ]
    .into_iter()
    .map(|event| engine.rules_for(relation, event))
    .collect::<Result<Vec<_>, SQLError>>()?
    .iter()
    .any(|rules| !rules.is_empty());
    if !has_rules {
        return Ok(());
    }
    Err(SQLError::Routine {
        sqlstate: "0A000".into(),
        message: format!(
            "cannot execute MERGE on relation \"{}\"",
            display_relation(relation)
        ),
    })
}

fn merge_uses_event(plan: &MergePlan, event: TriggerEvent) -> bool {
    plan.when_clauses.iter().any(|clause| match event {
        TriggerEvent::Insert => matches!(clause, MergeWhenPlan::InsertNotMatched { .. }),
        TriggerEvent::Update => matches!(
            clause,
            MergeWhenPlan::UpdateMatched { .. } | MergeWhenPlan::UpdateNotMatchedBySource { .. }
        ),
        TriggerEvent::Delete => matches!(
            clause,
            MergeWhenPlan::DeleteMatched { .. } | MergeWhenPlan::DeleteNotMatchedBySource { .. }
        ),
        TriggerEvent::Truncate => false,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MergeViewTargetPath {
    AutomaticRewrite,
    ViewTriggers,
}

pub(super) fn merge_view_target_path(
    engine: &Engine,
    plan: &MergePlan,
) -> Result<MergeViewTargetPath, SQLError> {
    let canonical = engine
        .try_resolve_view_name(&plan.target)
        .map_err(|error| SQLError::Internal(format!("resolve MERGE view: {error}")))?
        .ok_or_else(|| SQLError::UnknownTable(plan.target.clone()))?;
    let definition = engine
        .view_definition(&canonical)?
        .ok_or_else(|| SQLError::UnknownTable(plan.target.clone()))?;
    if definition.kind == crate::StoredViewKind::Materialized {
        return Err(SQLError::Routine {
            sqlstate: "0A000".into(),
            message: format!(
                "cannot execute MERGE on relation \"{}\"",
                display_relation(&canonical)
            ),
        });
    }
    validate_merge_rule_free(engine, &canonical)?;
    let automatic = view_updatability(engine, &canonical)?.automatic;
    let insert_trigger = instead_of_trigger_definition(engine, &canonical, TriggerEvent::Insert)?;
    let update_trigger = instead_of_trigger_definition(engine, &canonical, TriggerEvent::Update)?;
    let delete_trigger = instead_of_trigger_definition(engine, &canonical, TriggerEvent::Delete)?;
    let supported = ViewMutationCapabilities {
        insertable: automatic.insertable || insert_trigger,
        updatable: automatic.updatable || update_trigger,
        deletable: automatic.deletable || delete_trigger,
    };
    if let Some(error) = merge_action_capability_error(&canonical, &plan.when_clauses, supported) {
        return Err(error);
    }
    let mut uses_automatic = false;
    let mut uses_trigger = false;
    let mut has_action = false;
    for (event, trigger) in [
        (TriggerEvent::Insert, insert_trigger),
        (TriggerEvent::Update, update_trigger),
        (TriggerEvent::Delete, delete_trigger),
    ] {
        if !merge_uses_event(plan, event) {
            continue;
        }
        has_action = true;
        uses_trigger |= trigger;
        uses_automatic |= !trigger;
    }
    if uses_trigger && uses_automatic {
        return Err(SQLError::Routine {
            sqlstate: "0A000".into(),
            message: format!(
                "cannot merge into view \"{}\"",
                display_relation(&canonical)
            ),
        });
    }
    if uses_trigger || !has_action {
        Ok(MergeViewTargetPath::ViewTriggers)
    } else {
        Ok(MergeViewTargetPath::AutomaticRewrite)
    }
}

fn validate_public_view_targets<'a>(
    engine: &Engine,
    view: &str,
    columns: impl IntoIterator<Item = &'a str>,
) -> Result<(), SQLError> {
    let definition = engine
        .view_definition(view)?
        .ok_or_else(|| SQLError::UnknownTable(view.to_string()))?;
    let schema = engine.stored_view_schema(&definition)?;
    for column in columns {
        if !schema.has_unqualified_column(column) {
            return Err(SQLError::UnknownColumn(format!("{view}.{column}")));
        }
    }
    Ok(())
}

fn validate_direct_view_rule_path(
    engine: &Engine,
    view: &str,
    event: uqa_sql::ast::RuleEvent,
    operation: &str,
) -> Result<(), SQLError> {
    let rules = engine.rules_for(view, event)?;
    let has_conditional_instead = rules
        .iter()
        .any(|rule| rule.definition.instead && rule.definition.condition.is_some());
    let has_unconditional_instead = rules
        .iter()
        .any(|rule| rule.definition.instead && rule.definition.condition.is_none());
    if has_conditional_instead && !has_unconditional_instead {
        return Err(not_automatically_updatable(view, operation));
    }
    Ok(())
}

fn dml_analysis_scope(ctes: &[uqa_planner::CtePlan], subqueries: &[QueryPlan]) -> CteScope {
    let mut scope = CteScope::new_for_current_routine();
    for cte in ctes {
        scope.insert_deferred(cte.clone());
    }
    scope.scalar_subqueries = subqueries.to_vec();
    scope
}

fn public_view_row_schema(
    engine: &Engine,
    view: &str,
    target_qualifier: &str,
) -> Result<RowSchema, SQLError> {
    let definition = engine
        .view_definition(view)?
        .ok_or_else(|| SQLError::UnknownTable(view.to_string()))?;
    let schema = engine.stored_view_schema(&definition)?;
    let names = schema
        .columns()
        .iter()
        .enumerate()
        .map(|(position, column)| schema.public_name(position).unwrap_or(column).to_string())
        .collect();
    Ok(RowSchema::with_qualified_types(
        target_qualifier,
        names,
        schema.column_types().to_vec(),
    ))
}

fn collect_expression_subquery_ids<'a>(
    expressions: impl IntoIterator<Item = &'a ScalarExpr>,
) -> BTreeSet<usize> {
    let mut ids = BTreeSet::new();
    for expression in expressions {
        crate::sql::select::collect_subquery_ids(expression, &mut ids);
    }
    ids
}

fn dml_correlated_outer_schema(
    engine: &Engine,
    layer: &AutomaticViewLayer,
    target_qualifier: &str,
    source: Option<&RowSchema>,
    returning_aliases: Option<&ReturningAliases>,
    include_excluded: bool,
) -> Result<(RowSchema, BTreeSet<String>), SQLError> {
    let target = public_view_row_schema(engine, &layer.canonical_name, target_qualifier)?;
    let columns = target.columns().to_vec();
    let types = target.column_types().to_vec();
    let mut outer = target;
    let mut target_qualifiers = BTreeSet::from([target_qualifier.to_string()]);
    if include_excluded {
        let excluded = RowSchema::with_qualified_types("excluded", columns.clone(), types.clone());
        outer = RowSchema::join(&outer, &excluded, std::iter::empty());
        target_qualifiers.insert("excluded".into());
    }
    if let Some(aliases) = returning_aliases {
        let expression_scope = ExpressionScope {
            target_qualifier,
            returning_aliases: Some(aliases),
            source,
            include_excluded: false,
        };
        let mut identities = Vec::new();
        for alias in [&aliases.old, &aliases.new] {
            if !expression_scope.row_image_qualifier(alias) {
                continue;
            }
            target_qualifiers.insert(alias.clone());
            identities.extend(columns.iter().enumerate().map(|(position, column)| {
                (
                    ColumnIdentity::qualified(alias, column),
                    types[position].clone(),
                )
            }));
        }
        outer = RowSchema::with_typed_virtual_identities(&outer, &identities);
    }
    if let Some(source) = source {
        outer = RowSchema::join(&outer, source, std::iter::empty());
    }
    Ok((outer, target_qualifiers))
}

fn validate_correlated_subquery_ids(
    engine: &Engine,
    ctes: &[uqa_planner::CtePlan],
    subqueries: &[QueryPlan],
    ids: &BTreeSet<usize>,
    outer: &RowSchema,
    params: &[uqa_sql::SQLParam],
) -> Result<(), SQLError> {
    let scope = dml_analysis_scope(ctes, subqueries);
    for id in ids {
        let query = subqueries.get(*id).ok_or_else(|| {
            SQLError::Internal(format!("DML scalar subquery slot {id} is out of bounds"))
        })?;
        crate::sql::select::analyze_query_plan_schema(engine, query, params, &scope, Some(outer))?;
    }
    Ok(())
}

fn rewrite_correlated_scalar(
    context: &CorrelatedRewriteContext<'_>,
    expression: &mut ScalarExpr,
    shadowed_qualifiers: &BTreeSet<String>,
    shadowed_columns: &BTreeSet<String>,
    subqueries: &mut Vec<QueryPlan>,
) -> Result<(), SQLError> {
    let mut error = None;
    uqa_planner::rewrite_scalar_expression(expression, &mut |node| {
        if error.is_some() {
            return;
        }
        let replacement = match node {
            ScalarExpr::Column(column) if !shadowed_columns.contains(column) => {
                layer_column(context.layer, column)
                    .map(|mapping| {
                        embed_layer_expression(
                            context.engine,
                            &mapping.expression,
                            context.layer,
                            context.default_target_qualifier,
                            subqueries,
                        )
                    })
                    .or_else(|| {
                        let source = context.source?;
                        let position = source.unqualified_position(column)?;
                        let qualifier = source.identity(position)?.qualifier()?;
                        Some(Ok(ScalarExpr::QualifiedColumn {
                            qualifier: qualifier.to_string(),
                            column: column.clone(),
                        }))
                    })
            }
            ScalarExpr::QualifiedColumn { qualifier, column }
                if context.target_qualifiers.contains(qualifier)
                    && !shadowed_qualifiers.contains(qualifier) =>
            {
                layer_column(context.layer, column).map(|mapping| {
                    embed_layer_expression(
                        context.engine,
                        &mapping.expression,
                        context.layer,
                        qualifier,
                        subqueries,
                    )
                })
            }
            _ => None,
        };
        if let Some(replacement) = replacement {
            match replacement {
                Ok(replacement) => *node = replacement,
                Err(rewrite_error) => error = Some(rewrite_error),
            }
        }
    });
    error.map_or(Ok(()), Err)
}

fn schema_public_columns(schema: &RowSchema) -> BTreeSet<String> {
    schema
        .columns()
        .iter()
        .enumerate()
        .map(|(position, column)| schema.public_name(position).unwrap_or(column).to_string())
        .collect()
}

struct CorrelatedRewriteContext<'a> {
    engine: &'a Engine,
    layer: &'a AutomaticViewLayer,
    default_target_qualifier: &'a str,
    target_qualifiers: &'a BTreeSet<String>,
    source: Option<&'a RowSchema>,
    params: &'a [uqa_sql::SQLParam],
    outer: &'a RowSchema,
}

fn rewrite_correlated_query(
    context: &CorrelatedRewriteContext<'_>,
    query: &mut QueryPlan,
    scope: &CteScope,
    inherited_qualifier_shadows: &BTreeSet<String>,
    inherited_column_shadows: &BTreeSet<String>,
) -> Result<(), SQLError> {
    let mut query_scope = scope.clone();
    for cte in &query.ctes {
        query_scope.insert_deferred(cte.clone());
    }
    for cte in &mut query.ctes {
        rewrite_correlated_query(
            context,
            &mut cte.query,
            &query_scope,
            inherited_qualifier_shadows,
            inherited_column_shadows,
        )?;
    }
    match &mut query.root {
        RelationalPlan::QueryBlock(block) => {
            let local_schema = block
                .from
                .as_ref()
                .map(|source| {
                    crate::sql::select::analyze_source_plan_schema(
                        context.engine,
                        source,
                        context.params,
                        &query_scope,
                        Some(context.outer),
                    )
                })
                .transpose()?;
            let mut qualifier_shadows = inherited_qualifier_shadows.clone();
            if let Some(schema) = local_schema.as_ref() {
                qualifier_shadows.extend(
                    context
                        .target_qualifiers
                        .iter()
                        .filter(|qualifier| schema.has_qualifier(qualifier))
                        .cloned(),
                );
            }
            let mut column_shadows = inherited_column_shadows.clone();
            if let Some(schema) = local_schema.as_ref() {
                column_shadows.extend(schema_public_columns(schema));
            }
            let original_subquery_count = block.subqueries.len();
            for subquery in &mut block.subqueries[..original_subquery_count] {
                rewrite_correlated_query(
                    context,
                    subquery,
                    &query_scope,
                    &qualifier_shadows,
                    &column_shadows,
                )?;
            }
            let mut rewrite = |expression: &mut ScalarExpr| {
                rewrite_correlated_scalar(
                    context,
                    expression,
                    &qualifier_shadows,
                    &column_shadows,
                    &mut block.subqueries,
                )
            };
            for projection in &mut block.projections {
                rewrite(&mut projection.expr)?;
            }
            if let Some(predicate) = &mut block.r#where {
                rewrite(predicate)?;
            }
            for expression in &mut block.group_by {
                rewrite(expression)?;
            }
            for set in &mut block.grouping_sets {
                for expression in set {
                    rewrite(expression)?;
                }
            }
            if let Some(having) = &mut block.having {
                rewrite(having)?;
            }
            for order in &mut block.order_by {
                rewrite(&mut order.expr)?;
            }
            if let Some(limit) = &mut block.limit {
                rewrite(limit)?;
            }
            if let Some(offset) = &mut block.offset {
                rewrite(offset)?;
            }
            for expression in &mut block.distinct_on {
                rewrite(expression)?;
            }
        }
        RelationalPlan::SetOp {
            left,
            right,
            order_by,
            limit,
            offset,
            subqueries,
            ..
        } => {
            rewrite_correlated_query(
                context,
                left,
                &query_scope,
                inherited_qualifier_shadows,
                inherited_column_shadows,
            )?;
            rewrite_correlated_query(
                context,
                right,
                &query_scope,
                inherited_qualifier_shadows,
                inherited_column_shadows,
            )?;
            let original_subquery_count = subqueries.len();
            for subquery in &mut subqueries[..original_subquery_count] {
                rewrite_correlated_query(
                    context,
                    subquery,
                    &query_scope,
                    inherited_qualifier_shadows,
                    inherited_column_shadows,
                )?;
            }
            for order in order_by {
                rewrite_correlated_scalar(
                    context,
                    &mut order.expr,
                    inherited_qualifier_shadows,
                    inherited_column_shadows,
                    subqueries,
                )?;
            }
            for expression in [limit.as_deref_mut(), offset.as_deref_mut()]
                .into_iter()
                .flatten()
            {
                rewrite_correlated_scalar(
                    context,
                    expression,
                    inherited_qualifier_shadows,
                    inherited_column_shadows,
                    subqueries,
                )?;
            }
        }
        RelationalPlan::Values { rows, subqueries } => {
            let original_subquery_count = subqueries.len();
            for subquery in &mut subqueries[..original_subquery_count] {
                rewrite_correlated_query(
                    context,
                    subquery,
                    &query_scope,
                    inherited_qualifier_shadows,
                    inherited_column_shadows,
                )?;
            }
            for expression in rows.iter_mut().flatten() {
                rewrite_correlated_scalar(
                    context,
                    expression,
                    inherited_qualifier_shadows,
                    inherited_column_shadows,
                    subqueries,
                )?;
            }
        }
    }
    Ok(())
}

fn dml_source_schema(
    engine: &Engine,
    source: Option<&SourcePlan>,
    ctes: &[uqa_planner::CtePlan],
    subqueries: &[QueryPlan],
    params: &[uqa_sql::SQLParam],
) -> Result<Option<RowSchema>, SQLError> {
    let Some(source) = source else {
        return Ok(None);
    };
    let scope = dml_analysis_scope(ctes, subqueries);
    crate::sql::select::analyze_source_plan_schema(engine, source, params, &scope, None).map(Some)
}

fn insert_input_width(
    engine: &Engine,
    plan: &InsertPlan,
    params: &[uqa_sql::SQLParam],
) -> Result<usize, SQLError> {
    let Some(source) = plan.source.as_deref() else {
        return Ok(plan.rows.first().map_or(0, Vec::len));
    };
    let scope = dml_analysis_scope(&plan.ctes, &plan.subqueries);
    Ok(crate::sql::select::analyze_query_plan_schema(engine, source, params, &scope, None)?.len())
}

fn returning_subquery_ids(returning: &[ProjectionPlan]) -> BTreeSet<usize> {
    collect_expression_subquery_ids(returning.iter().map(|projection| &projection.expr))
}

fn merge_matched_subquery_ids(plan: &MergePlan) -> BTreeSet<usize> {
    let mut ids = collect_expression_subquery_ids(std::iter::once(&plan.join_condition));
    for clause in &plan.when_clauses {
        match clause {
            MergeWhenPlan::UpdateMatched {
                condition,
                assignments,
            } => {
                ids.extend(collect_expression_subquery_ids(condition.iter()));
                ids.extend(collect_expression_subquery_ids(
                    assignments.iter().map(|assignment| &assignment.value),
                ));
            }
            MergeWhenPlan::DeleteMatched { condition }
            | MergeWhenPlan::NothingMatched { condition } => {
                ids.extend(collect_expression_subquery_ids(condition.iter()));
            }
            _ => {}
        }
    }
    ids
}

fn merge_target_only_subquery_ids(plan: &MergePlan) -> BTreeSet<usize> {
    let mut ids = BTreeSet::new();
    for clause in &plan.when_clauses {
        match clause {
            MergeWhenPlan::UpdateNotMatchedBySource {
                condition,
                assignments,
            } => {
                ids.extend(collect_expression_subquery_ids(condition.iter()));
                ids.extend(collect_expression_subquery_ids(
                    assignments.iter().map(|assignment| &assignment.value),
                ));
            }
            MergeWhenPlan::DeleteNotMatchedBySource { condition }
            | MergeWhenPlan::NothingNotMatchedBySource { condition } => {
                ids.extend(collect_expression_subquery_ids(condition.iter()));
            }
            _ => {}
        }
    }
    ids
}

fn insert_conflict_subquery_ids(plan: &InsertPlan) -> BTreeSet<usize> {
    let Some(ConflictPlan {
        action:
            ConflictActionPlan::Update {
                assignments,
                predicate,
            },
        ..
    }) = &plan.on_conflict
    else {
        return BTreeSet::new();
    };
    let mut ids =
        collect_expression_subquery_ids(assignments.iter().map(|assignment| &assignment.value));
    ids.extend(collect_expression_subquery_ids(predicate.iter()));
    ids
}

fn update_ordinary_subquery_ids(plan: &UpdatePlan) -> BTreeSet<usize> {
    let mut ids = collect_expression_subquery_ids(
        plan.assignments.iter().map(|assignment| &assignment.value),
    );
    ids.extend(collect_expression_subquery_ids(plan.predicate.iter()));
    ids
}

fn delete_ordinary_subquery_ids(plan: &DeletePlan) -> BTreeSet<usize> {
    collect_expression_subquery_ids(plan.predicate.iter())
}

struct CorrelatedDmlContext<'a> {
    engine: &'a Engine,
    layer: &'a AutomaticViewLayer,
    target_qualifier: &'a str,
    source: Option<&'a RowSchema>,
    returning_aliases: Option<&'a ReturningAliases>,
    include_excluded: bool,
    ctes: &'a [uqa_planner::CtePlan],
    ids: &'a BTreeSet<usize>,
    params: &'a [uqa_sql::SQLParam],
}

fn validate_correlated_dml_context(
    context: CorrelatedDmlContext<'_>,
    subqueries: &[QueryPlan],
) -> Result<(), SQLError> {
    let (outer, _) = dml_correlated_outer_schema(
        context.engine,
        context.layer,
        context.target_qualifier,
        context.source,
        context.returning_aliases,
        context.include_excluded,
    )?;
    validate_correlated_subquery_ids(
        context.engine,
        context.ctes,
        subqueries,
        context.ids,
        &outer,
        context.params,
    )
}

fn rewrite_correlated_dml_context(
    context: CorrelatedDmlContext<'_>,
    subqueries: &mut [QueryPlan],
) -> Result<(), SQLError> {
    let (outer, target_qualifiers) = dml_correlated_outer_schema(
        context.engine,
        context.layer,
        context.target_qualifier,
        context.source,
        context.returning_aliases,
        context.include_excluded,
    )?;
    let scope = dml_analysis_scope(context.ctes, subqueries);
    let rewrite_context = CorrelatedRewriteContext {
        engine: context.engine,
        layer: context.layer,
        default_target_qualifier: context.target_qualifier,
        target_qualifiers: &target_qualifiers,
        source: context.source,
        params: context.params,
        outer: &outer,
    };
    for id in context.ids {
        let query = subqueries.get_mut(*id).ok_or_else(|| {
            SQLError::Internal(format!("DML scalar subquery slot {id} is out of bounds"))
        })?;
        rewrite_correlated_query(
            &rewrite_context,
            query,
            &scope,
            &BTreeSet::new(),
            &BTreeSet::new(),
        )?;
    }
    Ok(())
}

fn validate_update_expressions(
    engine: &Engine,
    plan: &UpdatePlan,
    layer: &AutomaticViewLayer,
    source: Option<&RowSchema>,
    params: &[uqa_sql::SQLParam],
) -> Result<(), SQLError> {
    validate_correlated_dml_context(
        CorrelatedDmlContext {
            engine,
            layer,
            target_qualifier: &plan.target_qualifier,
            source,
            returning_aliases: None,
            include_excluded: false,
            ctes: &plan.ctes,
            ids: &update_ordinary_subquery_ids(plan),
            params,
        },
        &plan.subqueries,
    )?;
    validate_correlated_dml_context(
        CorrelatedDmlContext {
            engine,
            layer,
            target_qualifier: &plan.target_qualifier,
            source,
            returning_aliases: Some(&plan.returning_aliases),
            include_excluded: false,
            ctes: &plan.ctes,
            ids: &returning_subquery_ids(&plan.returning),
            params,
        },
        &plan.subqueries,
    )?;
    let ordinary_scope = ExpressionScope {
        target_qualifier: &plan.target_qualifier,
        returning_aliases: None,
        source,
        include_excluded: false,
    };
    for assignment in &plan.assignments {
        validate_view_expression(&assignment.value, layer, ordinary_scope)?;
    }
    if let Some(predicate) = &plan.predicate {
        validate_view_expression(predicate, layer, ordinary_scope)?;
    }
    let returning_scope = ExpressionScope {
        returning_aliases: Some(&plan.returning_aliases),
        ..ordinary_scope
    };
    for projection in &plan.returning {
        validate_view_expression(&projection.expr, layer, returning_scope)?;
    }
    Ok(())
}

fn validate_delete_expressions(
    engine: &Engine,
    plan: &DeletePlan,
    layer: &AutomaticViewLayer,
    source: Option<&RowSchema>,
    params: &[uqa_sql::SQLParam],
) -> Result<(), SQLError> {
    validate_correlated_dml_context(
        CorrelatedDmlContext {
            engine,
            layer,
            target_qualifier: &plan.target_qualifier,
            source,
            returning_aliases: None,
            include_excluded: false,
            ctes: &plan.ctes,
            ids: &delete_ordinary_subquery_ids(plan),
            params,
        },
        &plan.subqueries,
    )?;
    validate_correlated_dml_context(
        CorrelatedDmlContext {
            engine,
            layer,
            target_qualifier: &plan.target_qualifier,
            source,
            returning_aliases: Some(&plan.returning_aliases),
            include_excluded: false,
            ctes: &plan.ctes,
            ids: &returning_subquery_ids(&plan.returning),
            params,
        },
        &plan.subqueries,
    )?;
    let ordinary_scope = ExpressionScope {
        target_qualifier: &plan.target_qualifier,
        returning_aliases: None,
        source,
        include_excluded: false,
    };
    if let Some(predicate) = &plan.predicate {
        validate_view_expression(predicate, layer, ordinary_scope)?;
    }
    let returning_scope = ExpressionScope {
        returning_aliases: Some(&plan.returning_aliases),
        ..ordinary_scope
    };
    for projection in &plan.returning {
        validate_view_expression(&projection.expr, layer, returning_scope)?;
    }
    Ok(())
}

fn validate_merge_expressions(
    engine: &Engine,
    plan: &MergePlan,
    layer: &AutomaticViewLayer,
    source: &RowSchema,
    params: &[uqa_sql::SQLParam],
) -> Result<(), SQLError> {
    validate_correlated_dml_context(
        CorrelatedDmlContext {
            engine,
            layer,
            target_qualifier: &plan.target_qualifier,
            source: Some(source),
            returning_aliases: None,
            include_excluded: false,
            ctes: &[],
            ids: &merge_matched_subquery_ids(plan),
            params,
        },
        &plan.subqueries,
    )?;
    validate_correlated_dml_context(
        CorrelatedDmlContext {
            engine,
            layer,
            target_qualifier: &plan.target_qualifier,
            source: None,
            returning_aliases: None,
            include_excluded: false,
            ctes: &[],
            ids: &merge_target_only_subquery_ids(plan),
            params,
        },
        &plan.subqueries,
    )?;
    validate_correlated_dml_context(
        CorrelatedDmlContext {
            engine,
            layer,
            target_qualifier: &plan.target_qualifier,
            source: Some(source),
            returning_aliases: Some(&plan.returning_aliases),
            include_excluded: false,
            ctes: &[],
            ids: &returning_subquery_ids(&plan.returning),
            params,
        },
        &plan.subqueries,
    )?;
    let matched_scope = ExpressionScope {
        target_qualifier: &plan.target_qualifier,
        returning_aliases: None,
        source: Some(source),
        include_excluded: false,
    };
    let target_only_scope = ExpressionScope {
        source: None,
        ..matched_scope
    };
    validate_view_expression(&plan.join_condition, layer, matched_scope)?;
    for clause in &plan.when_clauses {
        match clause {
            MergeWhenPlan::UpdateMatched {
                condition,
                assignments,
            } => {
                if let Some(condition) = condition {
                    validate_view_expression(condition, layer, matched_scope)?;
                }
                for assignment in assignments {
                    validate_view_expression(&assignment.value, layer, matched_scope)?;
                }
            }
            MergeWhenPlan::DeleteMatched { condition }
            | MergeWhenPlan::NothingMatched { condition } => {
                if let Some(condition) = condition {
                    validate_view_expression(condition, layer, matched_scope)?;
                }
            }
            MergeWhenPlan::UpdateNotMatchedBySource {
                condition,
                assignments,
            } => {
                if let Some(condition) = condition {
                    validate_view_expression(condition, layer, target_only_scope)?;
                }
                for assignment in assignments {
                    validate_view_expression(&assignment.value, layer, target_only_scope)?;
                }
            }
            MergeWhenPlan::DeleteNotMatchedBySource { condition }
            | MergeWhenPlan::NothingNotMatchedBySource { condition } => {
                if let Some(condition) = condition {
                    validate_view_expression(condition, layer, target_only_scope)?;
                }
            }
            MergeWhenPlan::InsertNotMatched { .. } | MergeWhenPlan::NothingNotMatched { .. } => {}
        }
    }
    let returning_scope = ExpressionScope {
        returning_aliases: Some(&plan.returning_aliases),
        ..matched_scope
    };
    for projection in &plan.returning {
        validate_view_expression(&projection.expr, layer, returning_scope)?;
    }
    Ok(())
}

fn validate_insert_expressions(
    engine: &Engine,
    plan: &InsertPlan,
    layer: &AutomaticViewLayer,
    params: &[uqa_sql::SQLParam],
) -> Result<(), SQLError> {
    validate_correlated_dml_context(
        CorrelatedDmlContext {
            engine,
            layer,
            target_qualifier: &plan.target_qualifier,
            source: None,
            returning_aliases: None,
            include_excluded: true,
            ctes: &plan.ctes,
            ids: &insert_conflict_subquery_ids(plan),
            params,
        },
        &plan.subqueries,
    )?;
    validate_correlated_dml_context(
        CorrelatedDmlContext {
            engine,
            layer,
            target_qualifier: &plan.target_qualifier,
            source: None,
            returning_aliases: Some(&plan.returning_aliases),
            include_excluded: false,
            ctes: &plan.ctes,
            ids: &returning_subquery_ids(&plan.returning),
            params,
        },
        &plan.subqueries,
    )?;
    if let Some(conflict) = &plan.on_conflict {
        if let ConflictActionPlan::Update {
            assignments,
            predicate,
        } = &conflict.action
        {
            let scope = ExpressionScope {
                target_qualifier: &plan.target_qualifier,
                returning_aliases: None,
                source: None,
                include_excluded: true,
            };
            for assignment in assignments {
                validate_view_expression(&assignment.value, layer, scope)?;
            }
            if let Some(predicate) = predicate {
                validate_view_expression(predicate, layer, scope)?;
            }
        }
    }
    let scope = ExpressionScope {
        target_qualifier: &plan.target_qualifier,
        returning_aliases: Some(&plan.returning_aliases),
        source: None,
        include_excluded: false,
    };
    for projection in &plan.returning {
        validate_view_expression(&projection.expr, layer, scope)?;
    }
    Ok(())
}

fn retarget_source_expression(
    expression: &ScalarExpr,
    layer: &AutomaticViewLayer,
    desired_qualifier: &str,
) -> ScalarExpr {
    let mut expression = expression.clone();
    uqa_planner::rewrite_scalar_expression(&mut expression, &mut |node| {
        let replacement = match node {
            ScalarExpr::Column(column) => Some(ScalarExpr::QualifiedColumn {
                qualifier: desired_qualifier.to_string(),
                column: column.clone(),
            }),
            ScalarExpr::QualifiedColumn { qualifier, column }
                if source_qualifier_matches(
                    qualifier,
                    &layer.source_qualifier,
                    &layer.source_name,
                ) =>
            {
                Some(ScalarExpr::QualifiedColumn {
                    qualifier: desired_qualifier.to_string(),
                    column: column.clone(),
                })
            }
            _ => None,
        };
        if let Some(replacement) = replacement {
            *node = replacement;
        }
    });
    expression
}

fn rewrite_target_expression(
    engine: &Engine,
    expression: &mut ScalarExpr,
    layer: &AutomaticViewLayer,
    scope: ExpressionScope<'_>,
    subqueries: &mut Vec<QueryPlan>,
) -> Result<(), SQLError> {
    let mut error = None;
    uqa_planner::rewrite_scalar_expression(expression, &mut |node| {
        if error.is_some() {
            return;
        }
        let replacement = match node {
            ScalarExpr::Column(column) => layer_column(layer, column)
                .map(|mapping| {
                    embed_layer_expression(
                        engine,
                        &mapping.expression,
                        layer,
                        scope.target_qualifier,
                        subqueries,
                    )
                })
                .or_else(|| {
                    let source = scope.source?;
                    let position = source.unqualified_position(column)?;
                    let identity = source.identity(position)?;
                    identity.qualifier().map(|qualifier| {
                        Ok(ScalarExpr::QualifiedColumn {
                            qualifier: qualifier.to_string(),
                            column: column.clone(),
                        })
                    })
                }),
            ScalarExpr::QualifiedColumn { qualifier, column }
                if scope.target_qualifier(qualifier) =>
            {
                layer_column(layer, column).map(|mapping| {
                    embed_layer_expression(
                        engine,
                        &mapping.expression,
                        layer,
                        qualifier,
                        subqueries,
                    )
                })
            }
            _ => None,
        };
        if let Some(replacement) = replacement {
            match replacement {
                Ok(replacement) => *node = replacement,
                Err(rewrite_error) => error = Some(rewrite_error),
            }
        }
    });
    error.map_or(Ok(()), Err)
}

fn top_level_view_column(
    expression: &ScalarExpr,
    layer: &AutomaticViewLayer,
    scope: ExpressionScope<'_>,
) -> Option<String> {
    match expression {
        ScalarExpr::Column(column) if layer_column(layer, column).is_some() => Some(column.clone()),
        ScalarExpr::QualifiedColumn { qualifier, column }
            if scope.target_qualifier(qualifier) && layer_column(layer, column).is_some() =>
        {
            Some(column.clone())
        }
        _ => None,
    }
}

fn rewrite_returning(
    engine: &Engine,
    returning: Vec<ProjectionPlan>,
    layer: &AutomaticViewLayer,
    target_qualifier: &str,
    returning_aliases: &ReturningAliases,
    source: Option<&RowSchema>,
    subqueries: &mut Vec<QueryPlan>,
) -> Result<(Vec<ProjectionPlan>, Vec<usize>), SQLError> {
    let scope = ExpressionScope {
        target_qualifier,
        returning_aliases: Some(returning_aliases),
        source,
        include_excluded: false,
    };
    let mut rewritten = Vec::new();
    let mut source_star_boundaries = Vec::new();
    for projection in returning {
        let bare_star = matches!(projection.expr, ScalarExpr::Star);
        let star_qualifier = match &projection.expr {
            ScalarExpr::Star => Some(target_qualifier),
            ScalarExpr::QualifiedStar(qualifier) if scope.target_qualifier(qualifier) => {
                Some(qualifier.as_str())
            }
            _ => None,
        };
        if let Some(qualifier) = star_qualifier {
            for column in &layer.columns {
                rewritten.push(ProjectionPlan {
                    expr: embed_layer_expression(
                        engine,
                        &column.expression,
                        layer,
                        qualifier,
                        subqueries,
                    )?,
                    alias: Some(column.name.clone()),
                });
            }
            if bare_star && source.is_some() {
                source_star_boundaries.push(rewritten.len());
            }
            continue;
        }
        let derived_alias = projection
            .alias
            .clone()
            .or_else(|| top_level_view_column(&projection.expr, layer, scope));
        let mut expression = projection.expr;
        rewrite_target_expression(engine, &mut expression, layer, scope, subqueries)?;
        rewritten.push(ProjectionPlan {
            expr: expression,
            alias: derived_alias,
        });
    }
    Ok((rewritten, source_star_boundaries))
}

fn rewrite_merge_returning(
    engine: &Engine,
    returning: Vec<ProjectionPlan>,
    layer: &AutomaticViewLayer,
    target_qualifier: &str,
    returning_aliases: &ReturningAliases,
    source: &RowSchema,
    subqueries: &mut Vec<QueryPlan>,
) -> Result<(Vec<ProjectionPlan>, Vec<usize>), SQLError> {
    let scope = ExpressionScope {
        target_qualifier,
        returning_aliases: Some(returning_aliases),
        source: Some(source),
        include_excluded: false,
    };
    let mut rewritten = Vec::new();
    let mut source_star_boundaries = Vec::new();
    for projection in returning {
        let bare_star = matches!(projection.expr, ScalarExpr::Star);
        let star_qualifier = match &projection.expr {
            ScalarExpr::Star => Some(target_qualifier),
            ScalarExpr::QualifiedStar(qualifier) if scope.target_qualifier(qualifier) => {
                Some(qualifier.as_str())
            }
            _ => None,
        };
        if let Some(qualifier) = star_qualifier {
            if bare_star {
                source_star_boundaries.push(rewritten.len());
            }
            for column in &layer.columns {
                rewritten.push(ProjectionPlan {
                    expr: embed_layer_expression(
                        engine,
                        &column.expression,
                        layer,
                        qualifier,
                        subqueries,
                    )?,
                    alias: Some(column.name.clone()),
                });
            }
            continue;
        }
        let derived_alias = projection
            .alias
            .clone()
            .or_else(|| top_level_view_column(&projection.expr, layer, scope));
        let mut expression = projection.expr;
        rewrite_target_expression(engine, &mut expression, layer, scope, subqueries)?;
        rewritten.push(ProjectionPlan {
            expr: expression,
            alias: derived_alias,
        });
    }
    Ok((rewritten, source_star_boundaries))
}

fn source_star_projections(source: &RowSchema, target_width: usize) -> Vec<ProjectionPlan> {
    source
        .columns()
        .iter()
        .enumerate()
        .filter(|(position, _)| source.wildcard_position_visible(*position))
        .map(|(position, column)| ProjectionPlan {
            expr: ScalarExpr::Position(target_width + position),
            alias: Some(source.public_name(position).unwrap_or(column).to_string()),
        })
        .collect()
}

fn expand_qualified_source_stars(
    returning: Vec<ProjectionPlan>,
    source: &RowSchema,
    target_width: usize,
) -> Result<Vec<ProjectionPlan>, SQLError> {
    let mut expanded = Vec::new();
    for projection in returning {
        let ScalarExpr::QualifiedStar(qualifier) = &projection.expr else {
            expanded.push(projection);
            continue;
        };
        let layout = source.qualified_star_position_layout(qualifier);
        if layout.is_empty() {
            return Err(SQLError::UnknownTable(qualifier.clone()));
        }
        for (column, logical, _, _) in layout {
            if logical.is_some_and(|position| !source.wildcard_position_visible(position)) {
                continue;
            }
            expanded.push(ProjectionPlan {
                expr: logical.map_or_else(
                    || ScalarExpr::QualifiedColumn {
                        qualifier: qualifier.clone(),
                        column: column.clone(),
                    },
                    |position| ScalarExpr::Position(target_width + position),
                ),
                alias: Some(column),
            });
        }
    }
    Ok(expanded)
}

fn dml_target_width(engine: &Engine, table: &str) -> Result<usize, SQLError> {
    let target_columns = relation_columns(engine, table)?;
    Ok(target_columns.len()
        + usize::from(
            !target_columns
                .iter()
                .any(|column| column == super::DOC_ID_COLUMN),
        )
        + 2)
}

fn bind_unqualified_source_positions(
    expression: &mut ScalarExpr,
    source: &RowSchema,
    target_width: usize,
) {
    uqa_planner::rewrite_scalar_expression(expression, &mut |node| {
        let ScalarExpr::Column(column) = node else {
            return;
        };
        let Some(position) = source.unqualified_position(column) else {
            return;
        };
        if source
            .identity(position)
            .is_some_and(|identity| identity.qualifier().is_none())
        {
            *node = ScalarExpr::Position(target_width + position);
        }
    });
}

fn finalize_source_returning(
    engine: &Engine,
    table: &str,
    returning: Vec<ProjectionPlan>,
    source: Option<&RowSchema>,
    source_star_boundaries: &[usize],
) -> Result<Vec<ProjectionPlan>, SQLError> {
    let Some(source) = source else {
        return Ok(returning);
    };
    let target_width = dml_target_width(engine, table)?;
    let source_star = source_star_projections(source, target_width);
    let mut returning = returning;
    let mut inserted = 0;
    for boundary in source_star_boundaries {
        let position = boundary + inserted;
        returning.splice(position..position, source_star.iter().cloned());
        inserted += source_star.len();
    }
    expand_qualified_source_stars(returning, source, target_width)
}

fn rewrite_existing_view_checks(
    engine: &Engine,
    checks: &mut [ViewCheckPlan],
    layer: &AutomaticViewLayer,
    target_qualifier: &str,
    subqueries: &mut Vec<QueryPlan>,
) -> Result<(), SQLError> {
    let scope = ExpressionScope {
        target_qualifier,
        returning_aliases: None,
        source: None,
        include_excluded: false,
    };
    for check in checks {
        rewrite_target_expression(engine, &mut check.predicate, layer, scope, subqueries)?;
    }
    Ok(())
}

fn add_check_option(
    engine: &Engine,
    checks: &mut Vec<ViewCheckPlan>,
    layer: &AutomaticViewLayer,
    target_qualifier: &str,
    cascaded: &mut bool,
    subqueries: &mut Vec<QueryPlan>,
) -> Result<(), SQLError> {
    let check_current = *cascaded || layer.check_option != ViewCheckOption::None;
    *cascaded |= layer.check_option == ViewCheckOption::Cascaded;
    if check_current {
        if let Some(predicate) = &layer.predicate {
            checks.insert(
                0,
                ViewCheckPlan {
                    view: layer.canonical_name.clone(),
                    predicate: embed_layer_expression(
                        engine,
                        predicate,
                        layer,
                        target_qualifier,
                        subqueries,
                    )?,
                },
            );
        }
    }
    Ok(())
}

fn combine_view_predicate(
    engine: &Engine,
    current: Option<ScalarExpr>,
    layer: &AutomaticViewLayer,
    target_qualifier: &str,
    subqueries: &mut Vec<QueryPlan>,
) -> Result<Option<ScalarExpr>, SQLError> {
    let view = layer
        .predicate
        .as_ref()
        .map(|predicate| {
            embed_layer_expression(engine, predicate, layer, target_qualifier, subqueries)
        })
        .transpose()?;
    Ok(match (view, current) {
        (Some(view), Some(current)) => Some(ScalarExpr::And(vec![view, current])),
        (Some(view), None) => Some(view),
        (None, current) => current,
    })
}

fn instead_of_trigger_definition(
    engine: &Engine,
    view: &str,
    event: TriggerEvent,
) -> Result<bool, SQLError> {
    engine.has_trigger_definition(view, TriggerTiming::InsteadOf, event, true)
}

fn record_view_rule_relation(
    engine: &Engine,
    relations: &mut Vec<String>,
    layer: &AutomaticViewLayer,
    event: uqa_sql::ast::RuleEvent,
) -> Result<bool, SQLError> {
    let has_rules = !engine.rules_for(&layer.canonical_name, event)?.is_empty();
    if has_rules && !relations.contains(&layer.canonical_name) {
        relations.push(layer.canonical_name.clone());
    }
    Ok(has_rules)
}

fn preserve_view_rule_returning(
    target: &mut Option<ViewRuleReturningPlan>,
    relation: &str,
    target_qualifier: &str,
    returning: &[ProjectionPlan],
    aliases: &ReturningAliases,
    subqueries: &[QueryPlan],
) {
    if target.is_none() {
        *target = Some(ViewRuleReturningPlan {
            relation: relation.to_string(),
            target_qualifier: target_qualifier.to_string(),
            returning: returning.to_vec(),
            aliases: aliases.clone(),
            subqueries: subqueries.to_vec(),
        });
    }
}

fn stored_view_document_row(
    engine: &Engine,
    view: &str,
    qualifier: &str,
    document: &Document,
) -> Result<OwnedPhysicalRow, SQLError> {
    let definition = engine
        .view_definition(view)?
        .ok_or_else(|| SQLError::UnknownTable(view.to_string()))?;
    let schema = engine.stored_view_schema(&definition)?;
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

struct AutomaticViewRuleProjection<'a> {
    engine: &'a Engine,
    document_relation: Option<&'a str>,
    storage_table: Option<&'a str>,
    doc_id: Option<DocId>,
    document: &'a Document,
    params: &'a [uqa_sql::SQLParam],
    scope: &'a CteScope,
}

fn automatic_view_rule_document_inner(
    projection: &AutomaticViewRuleProjection<'_>,
    view: &str,
    required_columns: &BTreeSet<String>,
    visited: &mut BTreeSet<String>,
) -> Result<Document, SQLError> {
    let layer = automatic_view_layer(projection.engine, view)?.ok_or_else(|| {
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
            source_dependencies.extend(relation_columns(projection.engine, &layer.source_name)?);
        }
        uqa_planner::rewrite_scalar_expression(&mut expression, &mut |node| match node {
            ScalarExpr::Column(column) => {
                source_dependencies.insert(column.clone());
            }
            ScalarExpr::QualifiedColumn { qualifier, column }
                if qualifier.eq_ignore_ascii_case(&layer.source_qualifier) =>
            {
                source_dependencies.insert(column.clone());
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
        .engine
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
            projection.engine,
            &layer.source_name,
            &layer.source_qualifier,
            source_document,
        )?
    } else {
        super::dml_target_row_for_storage_optional(
            projection.engine,
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
        let value = super::eval_mutation_expr(
            projection.engine,
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

pub(in crate::sql) struct AutomaticViewRuleDocument<'a> {
    pub(in crate::sql) engine: &'a Engine,
    pub(in crate::sql) view: &'a str,
    pub(in crate::sql) document_relation: Option<&'a str>,
    pub(in crate::sql) storage_table: Option<&'a str>,
    pub(in crate::sql) doc_id: Option<DocId>,
    pub(in crate::sql) document: &'a Document,
    pub(in crate::sql) required_columns: &'a BTreeSet<String>,
    pub(in crate::sql) params: &'a [uqa_sql::SQLParam],
    pub(in crate::sql) scope: &'a CteScope,
}

pub(super) fn automatic_view_rule_document(
    request: AutomaticViewRuleDocument<'_>,
) -> Result<Document, SQLError> {
    let projection = AutomaticViewRuleProjection {
        engine: request.engine,
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

pub(in crate::sql) fn has_instead_of_trigger(
    engine: &Engine,
    view: &str,
    event: TriggerEvent,
) -> Result<bool, SQLError> {
    let Some(canonical) = engine
        .try_resolve_view_name(view)
        .map_err(|error| SQLError::Internal(format!("resolve DML view `{view}`: {error}")))?
    else {
        return Ok(false);
    };
    instead_of_trigger_definition(engine, &canonical, event)
}

pub(super) fn rewrite_insert_to_base(
    engine: &Engine,
    statement: &InsertPlan,
    params: &[uqa_sql::SQLParam],
) -> Result<InsertPlan, SQLError> {
    validate_public_view_targets(
        engine,
        &statement.table,
        statement.columns.iter().map(String::as_str),
    )?;
    validate_public_insert_contract(engine, statement)?;
    let Some(initial_layer) = automatic_view_layer(engine, &statement.table)? else {
        return Err(not_automatically_updatable(&statement.table, "INSERT"));
    };
    validate_insert_targets(&initial_layer, statement)?;
    validate_direct_view_rule_path(
        engine,
        &initial_layer.canonical_name,
        uqa_sql::ast::RuleEvent::Insert,
        "INSERT",
    )?;
    if !view_updatability(engine, &statement.table)?
        .automatic
        .insertable
    {
        return Err(not_automatically_updatable(&statement.table, "INSERT"));
    }
    let mut plan = statement.clone();
    let mut implicit_width = if statement.columns.is_empty() {
        Some(insert_input_width(engine, statement, params)?)
    } else {
        None
    };
    let mut cascaded = false;
    let mut visited = BTreeSet::new();
    let mut rewrite_suppressed = false;
    loop {
        let Some(layer) = automatic_view_layer(engine, &plan.table)? else {
            if active_unconditional_instead_rule(
                engine,
                &plan.table,
                uqa_sql::ast::RuleEvent::Insert,
            )? {
                break;
            }
            return Err(not_automatically_updatable(&plan.table, "INSERT"));
        };
        if !visited.insert(layer.canonical_name.clone()) {
            return Err(SQLError::Internal(format!(
                "cycle while rewriting automatically updatable view `{}`",
                layer.canonical_name
            )));
        }
        if !rewrite_suppressed {
            validate_direct_view_rule_path(
                engine,
                &layer.canonical_name,
                uqa_sql::ast::RuleEvent::Insert,
                "INSERT",
            )?;
        }
        if !rewrite_suppressed
            && visited.len() > 1
            && instead_of_trigger_definition(engine, &layer.canonical_name, TriggerEvent::Insert)?
        {
            return Err(not_automatically_updatable(&layer.canonical_name, "INSERT"));
        }
        let has_view_rules = if rewrite_suppressed {
            false
        } else {
            record_view_rule_relation(
                engine,
                &mut plan.view_rule_relations,
                &layer,
                uqa_sql::ast::RuleEvent::Insert,
            )?
        };
        let layer_suppresses = has_view_rules
            && crate::sql::rules::relation_suppresses_original_query(
                engine,
                &layer.canonical_name,
                uqa_sql::ast::RuleEvent::Insert,
            )?;
        if has_view_rules
            && crate::sql::rules::relation_has_returning_provider(
                engine,
                &layer.canonical_name,
                uqa_sql::ast::RuleEvent::Insert,
            )?
        {
            preserve_view_rule_returning(
                &mut plan.view_rule_returning,
                &layer.canonical_name,
                &plan.target_qualifier,
                &plan.returning,
                &plan.returning_aliases,
                &plan.subqueries,
            );
        }
        let target_qualifier = plan.target_qualifier.clone();
        if visited.len() == 1 {
            validate_insert_expressions(engine, &plan, &layer, params)?;
        }
        let conflict_subquery_ids = insert_conflict_subquery_ids(&plan);
        rewrite_correlated_dml_context(
            CorrelatedDmlContext {
                engine,
                layer: &layer,
                target_qualifier: &plan.target_qualifier,
                source: None,
                returning_aliases: None,
                include_excluded: true,
                ctes: &plan.ctes,
                ids: &conflict_subquery_ids,
                params,
            },
            &mut plan.subqueries,
        )?;
        let returning_subquery_ids = returning_subquery_ids(&plan.returning);
        rewrite_correlated_dml_context(
            CorrelatedDmlContext {
                engine,
                layer: &layer,
                target_qualifier: &plan.target_qualifier,
                source: None,
                returning_aliases: Some(&plan.returning_aliases),
                include_excluded: false,
                ctes: &plan.ctes,
                ids: &returning_subquery_ids,
                params,
            },
            &mut plan.subqueries,
        )?;
        let supplied_columns = if let Some(width) = implicit_width.take() {
            layer
                .columns
                .iter()
                .take(width)
                .map(|column| column.name.clone())
                .collect::<Vec<_>>()
        } else {
            plan.columns.clone()
        };
        let columns = if rewrite_suppressed || layer_suppresses {
            supplied_columns.clone()
        } else {
            supplied_columns
                .iter()
                .map(|column| writable_column(&layer, column, "INSERT"))
                .collect::<Result<Vec<_>, _>>()?
        };
        if has_view_rules {
            plan.view_rule_insert_plans.push(ViewRuleInsertPlan {
                relation: layer.canonical_name.clone(),
                supplied_columns,
                input_columns: Vec::new(),
            });
        }
        validate_mapped_columns(&columns, duplicate_assignment)?;
        if let Some(conflict) = &mut plan.on_conflict {
            conflict.conflict_columns = conflict
                .conflict_columns
                .iter()
                .map(|column| writable_column(&layer, column, "INSERT"))
                .collect::<Result<Vec<_>, _>>()?;
            if let ConflictActionPlan::Update {
                assignments,
                predicate,
            } = &mut conflict.action
            {
                let scope = ExpressionScope {
                    target_qualifier: &target_qualifier,
                    returning_aliases: None,
                    source: None,
                    include_excluded: true,
                };
                for assignment in assignments.iter_mut() {
                    assignment.column = writable_column(&layer, &assignment.column, "UPDATE")?;
                    rewrite_target_expression(
                        engine,
                        &mut assignment.value,
                        &layer,
                        scope,
                        &mut plan.subqueries,
                    )?;
                }
                let mapped = assignments
                    .iter()
                    .map(|assignment| assignment.column.clone())
                    .collect::<Vec<_>>();
                validate_mapped_columns(&mapped, duplicate_assignment)?;
                if let Some(predicate) = predicate {
                    rewrite_target_expression(
                        engine,
                        predicate,
                        &layer,
                        scope,
                        &mut plan.subqueries,
                    )?;
                }
            }
        }
        rewrite_existing_view_checks(
            engine,
            &mut plan.view_checks,
            &layer,
            &target_qualifier,
            &mut plan.subqueries,
        )?;
        let (returning, _) = rewrite_returning(
            engine,
            plan.returning,
            &layer,
            &target_qualifier,
            &plan.returning_aliases,
            None,
            &mut plan.subqueries,
        )?;
        plan.returning = returning;
        add_check_option(
            engine,
            &mut plan.view_checks,
            &layer,
            &target_qualifier,
            &mut cascaded,
            &mut plan.subqueries,
        )?;
        plan.columns = columns;
        plan.table = layer.source_name;
        plan.include_descendants = true;
        rewrite_suppressed |= layer_suppresses;
        if !super::view_triggers::target_is_view(engine, &plan.table)? {
            break;
        }
    }
    for insert_plan in &mut plan.view_rule_insert_plans {
        insert_plan.input_columns.clone_from(&plan.columns);
    }
    Ok(plan)
}

pub(super) fn rewrite_update_to_base(
    engine: &Engine,
    statement: &UpdatePlan,
    params: &[uqa_sql::SQLParam],
) -> Result<UpdatePlan, SQLError> {
    validate_public_view_targets(
        engine,
        &statement.table,
        statement
            .assignments
            .iter()
            .map(|assignment| assignment.column.as_str()),
    )?;
    let source_schema = dml_source_schema(
        engine,
        statement.source.as_deref(),
        &statement.ctes,
        &statement.subqueries,
        params,
    )?;
    validate_public_update_contract(engine, statement, source_schema.as_ref())?;
    let Some(initial_layer) = automatic_view_layer(engine, &statement.table)? else {
        return Err(not_automatically_updatable(&statement.table, "UPDATE"));
    };
    validate_update_targets(&initial_layer, statement)?;
    validate_direct_view_rule_path(
        engine,
        &initial_layer.canonical_name,
        uqa_sql::ast::RuleEvent::Update,
        "UPDATE",
    )?;
    if !view_updatability(engine, &statement.table)?
        .automatic
        .updatable
    {
        return Err(not_automatically_updatable(&statement.table, "UPDATE"));
    }
    let mut plan = statement.clone();
    let mut cascaded = false;
    let mut visited = BTreeSet::new();
    let mut source_star_boundaries = Vec::new();
    let mut rewrite_suppressed = false;
    loop {
        let Some(layer) = automatic_view_layer(engine, &plan.table)? else {
            if active_unconditional_instead_rule(
                engine,
                &plan.table,
                uqa_sql::ast::RuleEvent::Update,
            )? {
                break;
            }
            return Err(not_automatically_updatable(&plan.table, "UPDATE"));
        };
        if !visited.insert(layer.canonical_name.clone()) {
            return Err(SQLError::Internal(format!(
                "cycle while rewriting automatically updatable view `{}`",
                layer.canonical_name
            )));
        }
        if !rewrite_suppressed {
            validate_direct_view_rule_path(
                engine,
                &layer.canonical_name,
                uqa_sql::ast::RuleEvent::Update,
                "UPDATE",
            )?;
        }
        if !rewrite_suppressed
            && visited.len() > 1
            && instead_of_trigger_definition(engine, &layer.canonical_name, TriggerEvent::Update)?
        {
            return Err(not_automatically_updatable(&layer.canonical_name, "UPDATE"));
        }
        let has_view_rules = if rewrite_suppressed {
            false
        } else {
            record_view_rule_relation(
                engine,
                &mut plan.view_rule_relations,
                &layer,
                uqa_sql::ast::RuleEvent::Update,
            )?
        };
        let layer_suppresses = has_view_rules
            && crate::sql::rules::relation_suppresses_original_query(
                engine,
                &layer.canonical_name,
                uqa_sql::ast::RuleEvent::Update,
            )?;
        if has_view_rules
            && crate::sql::rules::relation_has_returning_provider(
                engine,
                &layer.canonical_name,
                uqa_sql::ast::RuleEvent::Update,
            )?
        {
            preserve_view_rule_returning(
                &mut plan.view_rule_returning,
                &layer.canonical_name,
                &plan.target_qualifier,
                &plan.returning,
                &plan.returning_aliases,
                &plan.subqueries,
            );
        }
        if has_view_rules {
            plan.view_rule_update_plans.push(ViewRuleUpdatePlan {
                relation: layer.canonical_name.clone(),
                assigned_columns: plan
                    .assignments
                    .iter()
                    .map(|assignment| assignment.column.clone())
                    .collect(),
                input_columns: Vec::new(),
            });
        }
        let target_qualifier = plan.target_qualifier.clone();
        if visited.len() == 1 {
            validate_update_expressions(engine, &plan, &layer, source_schema.as_ref(), params)?;
        }
        let ordinary_subquery_ids = update_ordinary_subquery_ids(&plan);
        rewrite_correlated_dml_context(
            CorrelatedDmlContext {
                engine,
                layer: &layer,
                target_qualifier: &target_qualifier,
                source: source_schema.as_ref(),
                returning_aliases: None,
                include_excluded: false,
                ctes: &plan.ctes,
                ids: &ordinary_subquery_ids,
                params,
            },
            &mut plan.subqueries,
        )?;
        let returning_subquery_ids = returning_subquery_ids(&plan.returning);
        rewrite_correlated_dml_context(
            CorrelatedDmlContext {
                engine,
                layer: &layer,
                target_qualifier: &target_qualifier,
                source: source_schema.as_ref(),
                returning_aliases: Some(&plan.returning_aliases),
                include_excluded: false,
                ctes: &plan.ctes,
                ids: &returning_subquery_ids,
                params,
            },
            &mut plan.subqueries,
        )?;
        let ordinary_scope = ExpressionScope {
            target_qualifier: &target_qualifier,
            returning_aliases: None,
            source: source_schema.as_ref(),
            include_excluded: false,
        };
        for (position, AssignmentPlan { column, value }) in plan.assignments.iter_mut().enumerate()
        {
            rewrite_target_expression(engine, value, &layer, ordinary_scope, &mut plan.subqueries)?;
            if layer_suppresses {
                *column = format!("\0uqa_view_rule_update_{position}");
            } else if !rewrite_suppressed {
                *column = writable_column(&layer, column, "UPDATE")?;
            }
        }
        let mapped = plan
            .assignments
            .iter()
            .map(|assignment| assignment.column.clone())
            .collect::<Vec<_>>();
        validate_mapped_columns(&mapped, duplicate_assignment)?;
        if let Some(predicate) = &mut plan.predicate {
            rewrite_target_expression(
                engine,
                predicate,
                &layer,
                ordinary_scope,
                &mut plan.subqueries,
            )?;
        }
        rewrite_existing_view_checks(
            engine,
            &mut plan.view_checks,
            &layer,
            &target_qualifier,
            &mut plan.subqueries,
        )?;
        let (returning, boundaries) = rewrite_returning(
            engine,
            plan.returning,
            &layer,
            &target_qualifier,
            &plan.returning_aliases,
            source_schema.as_ref(),
            &mut plan.subqueries,
        )?;
        plan.returning = returning;
        if visited.len() == 1 {
            source_star_boundaries = boundaries;
        }
        plan.predicate = combine_view_predicate(
            engine,
            plan.predicate,
            &layer,
            &target_qualifier,
            &mut plan.subqueries,
        )?;
        add_check_option(
            engine,
            &mut plan.view_checks,
            &layer,
            &target_qualifier,
            &mut cascaded,
            &mut plan.subqueries,
        )?;
        plan.table = layer.source_name;
        plan.include_descendants = layer.source_include_descendants;
        rewrite_suppressed |= layer_suppresses;
        if !super::view_triggers::target_is_view(engine, &plan.table)? {
            break;
        }
    }
    let input_columns = plan
        .assignments
        .iter()
        .map(|assignment| assignment.column.clone())
        .collect::<Vec<_>>();
    for update_plan in &mut plan.view_rule_update_plans {
        update_plan.input_columns.clone_from(&input_columns);
    }
    if let Some(source) = source_schema.as_ref() {
        let target_width = dml_target_width(engine, &plan.table)?;
        for assignment in &mut plan.assignments {
            bind_unqualified_source_positions(&mut assignment.value, source, target_width);
        }
        if let Some(predicate) = &mut plan.predicate {
            bind_unqualified_source_positions(predicate, source, target_width);
        }
        for projection in &mut plan.returning {
            bind_unqualified_source_positions(&mut projection.expr, source, target_width);
        }
    }
    plan.returning = finalize_source_returning(
        engine,
        &plan.table,
        plan.returning,
        source_schema.as_ref(),
        &source_star_boundaries,
    )?;
    Ok(plan)
}

pub(super) fn rewrite_delete_to_base(
    engine: &Engine,
    statement: &DeletePlan,
    params: &[uqa_sql::SQLParam],
) -> Result<DeletePlan, SQLError> {
    let source_schema = dml_source_schema(
        engine,
        statement.source.as_deref(),
        &statement.ctes,
        &statement.subqueries,
        params,
    )?;
    validate_public_delete_contract(engine, statement, source_schema.as_ref())?;
    validate_direct_view_rule_path(
        engine,
        &statement.table,
        uqa_sql::ast::RuleEvent::Delete,
        "DELETE",
    )?;
    let mut plan = statement.clone();
    let mut visited = BTreeSet::new();
    let mut source_star_boundaries = Vec::new();
    loop {
        let Some(layer) = automatic_view_layer(engine, &plan.table)? else {
            if active_unconditional_instead_rule(
                engine,
                &plan.table,
                uqa_sql::ast::RuleEvent::Delete,
            )? {
                break;
            }
            return Err(not_automatically_updatable(&plan.table, "DELETE"));
        };
        if !visited.insert(layer.canonical_name.clone()) {
            return Err(SQLError::Internal(format!(
                "cycle while rewriting automatically updatable view `{}`",
                layer.canonical_name
            )));
        }
        validate_direct_view_rule_path(
            engine,
            &layer.canonical_name,
            uqa_sql::ast::RuleEvent::Delete,
            "DELETE",
        )?;
        if visited.len() > 1
            && instead_of_trigger_definition(engine, &layer.canonical_name, TriggerEvent::Delete)?
        {
            return Err(not_automatically_updatable(&layer.canonical_name, "DELETE"));
        }
        if record_view_rule_relation(
            engine,
            &mut plan.view_rule_relations,
            &layer,
            uqa_sql::ast::RuleEvent::Delete,
        )? && crate::sql::rules::relation_has_returning_provider(
            engine,
            &layer.canonical_name,
            uqa_sql::ast::RuleEvent::Delete,
        )? {
            preserve_view_rule_returning(
                &mut plan.view_rule_returning,
                &layer.canonical_name,
                &plan.target_qualifier,
                &plan.returning,
                &plan.returning_aliases,
                &plan.subqueries,
            );
        }
        let target_qualifier = plan.target_qualifier.clone();
        if visited.len() == 1 {
            validate_delete_expressions(engine, &plan, &layer, source_schema.as_ref(), params)?;
        }
        let ordinary_subquery_ids = delete_ordinary_subquery_ids(&plan);
        rewrite_correlated_dml_context(
            CorrelatedDmlContext {
                engine,
                layer: &layer,
                target_qualifier: &target_qualifier,
                source: source_schema.as_ref(),
                returning_aliases: None,
                include_excluded: false,
                ctes: &plan.ctes,
                ids: &ordinary_subquery_ids,
                params,
            },
            &mut plan.subqueries,
        )?;
        let returning_subquery_ids = returning_subquery_ids(&plan.returning);
        rewrite_correlated_dml_context(
            CorrelatedDmlContext {
                engine,
                layer: &layer,
                target_qualifier: &target_qualifier,
                source: source_schema.as_ref(),
                returning_aliases: Some(&plan.returning_aliases),
                include_excluded: false,
                ctes: &plan.ctes,
                ids: &returning_subquery_ids,
                params,
            },
            &mut plan.subqueries,
        )?;
        let ordinary_scope = ExpressionScope {
            target_qualifier: &target_qualifier,
            returning_aliases: None,
            source: source_schema.as_ref(),
            include_excluded: false,
        };
        if let Some(predicate) = &mut plan.predicate {
            rewrite_target_expression(
                engine,
                predicate,
                &layer,
                ordinary_scope,
                &mut plan.subqueries,
            )?;
        }
        let (returning, boundaries) = rewrite_returning(
            engine,
            plan.returning,
            &layer,
            &target_qualifier,
            &plan.returning_aliases,
            source_schema.as_ref(),
            &mut plan.subqueries,
        )?;
        plan.returning = returning;
        if visited.len() == 1 {
            source_star_boundaries = boundaries;
        }
        plan.predicate = combine_view_predicate(
            engine,
            plan.predicate,
            &layer,
            &target_qualifier,
            &mut plan.subqueries,
        )?;
        plan.table = layer.source_name;
        plan.include_descendants = layer.source_include_descendants;
        if !super::view_triggers::target_is_view(engine, &plan.table)? {
            break;
        }
    }
    if let Some(source) = source_schema.as_ref() {
        let target_width = dml_target_width(engine, &plan.table)?;
        if let Some(predicate) = &mut plan.predicate {
            bind_unqualified_source_positions(predicate, source, target_width);
        }
        for projection in &mut plan.returning {
            bind_unqualified_source_positions(&mut projection.expr, source, target_width);
        }
    }
    plan.returning = finalize_source_returning(
        engine,
        &plan.table,
        plan.returning,
        source_schema.as_ref(),
        &source_star_boundaries,
    )?;
    Ok(plan)
}

pub(super) fn rewrite_merge_to_base(
    engine: &Engine,
    statement: &MergePlan,
    params: &[uqa_sql::SQLParam],
) -> Result<MergePlan, SQLError> {
    if super::view_triggers::target_view_kind(engine, &statement.target)?
        == Some(StoredViewKind::Materialized)
    {
        return Err(SQLError::Routine {
            sqlstate: "0A000".into(),
            message: format!(
                "cannot execute MERGE on relation \"{}\"",
                display_relation(&statement.target)
            ),
        });
    }
    let analysis_scope = dml_analysis_scope(&[], &statement.subqueries);
    let source_schema = crate::sql::select::analyze_source_plan_schema(
        engine,
        &statement.source,
        params,
        &analysis_scope,
        None,
    )?;
    validate_public_merge_targets(engine, statement)?;
    validate_public_merge_contract(engine, statement, &source_schema)?;
    if merge_view_target_path(engine, statement)? != MergeViewTargetPath::AutomaticRewrite {
        return Err(SQLError::Internal(
            "automatic MERGE rewrite selected for a view-trigger target".into(),
        ));
    }
    let Some(initial_layer) = automatic_view_layer(engine, &statement.target)? else {
        return Err(merge_action_capability_error(
            &statement.target,
            &statement.when_clauses,
            ViewMutationCapabilities::default(),
        )
        .unwrap_or_else(|| not_automatically_updatable(&statement.target, "MERGE")));
    };
    validate_merge_targets(&initial_layer, statement)?;
    validate_merge_expressions(engine, statement, &initial_layer, &source_schema, params)?;
    if let Some(error) = merge_action_capability_error(
        &statement.target,
        &statement.when_clauses,
        view_updatability(engine, &statement.target)?.automatic,
    ) {
        return Err(error);
    }

    let mut plan = statement.clone();
    let mut cascaded = false;
    let mut visited = BTreeSet::new();
    let mut source_star_boundaries = Vec::new();
    loop {
        if !visited.is_empty()
            && merge_view_target_path(engine, &plan)? == MergeViewTargetPath::ViewTriggers
        {
            break;
        }
        let Some(layer) = automatic_view_layer(engine, &plan.target)? else {
            return Err(merge_action_capability_error(
                &plan.target,
                &plan.when_clauses,
                ViewMutationCapabilities::default(),
            )
            .unwrap_or_else(|| not_automatically_updatable(&plan.target, "MERGE")));
        };
        if !visited.insert(layer.canonical_name.clone()) {
            return Err(SQLError::Internal(format!(
                "cycle while rewriting automatically updatable view `{}`",
                layer.canonical_name
            )));
        }
        validate_merge_targets(&layer, &plan)?;

        let matched_subqueries = merge_matched_subquery_ids(&plan);
        rewrite_correlated_dml_context(
            CorrelatedDmlContext {
                engine,
                layer: &layer,
                target_qualifier: &plan.target_qualifier,
                source: Some(&source_schema),
                returning_aliases: None,
                include_excluded: false,
                ctes: &[],
                ids: &matched_subqueries,
                params,
            },
            &mut plan.subqueries,
        )?;
        let target_only_subqueries = merge_target_only_subquery_ids(&plan);
        rewrite_correlated_dml_context(
            CorrelatedDmlContext {
                engine,
                layer: &layer,
                target_qualifier: &plan.target_qualifier,
                source: None,
                returning_aliases: None,
                include_excluded: false,
                ctes: &[],
                ids: &target_only_subqueries,
                params,
            },
            &mut plan.subqueries,
        )?;
        let returning_subqueries = returning_subquery_ids(&plan.returning);
        rewrite_correlated_dml_context(
            CorrelatedDmlContext {
                engine,
                layer: &layer,
                target_qualifier: &plan.target_qualifier,
                source: Some(&source_schema),
                returning_aliases: Some(&plan.returning_aliases),
                include_excluded: false,
                ctes: &[],
                ids: &returning_subqueries,
                params,
            },
            &mut plan.subqueries,
        )?;
        let matched_scope = ExpressionScope {
            target_qualifier: &plan.target_qualifier,
            returning_aliases: None,
            source: Some(&source_schema),
            include_excluded: false,
        };
        let target_only_scope = ExpressionScope {
            source: None,
            ..matched_scope
        };
        rewrite_target_expression(
            engine,
            &mut plan.join_condition,
            &layer,
            matched_scope,
            &mut plan.subqueries,
        )?;
        if let Some(predicate) = &mut plan.target_predicate {
            rewrite_target_expression(
                engine,
                predicate,
                &layer,
                target_only_scope,
                &mut plan.subqueries,
            )?;
        }
        for clause in &mut plan.when_clauses {
            match clause {
                MergeWhenPlan::UpdateMatched {
                    condition,
                    assignments,
                } => {
                    if let Some(condition) = condition {
                        rewrite_target_expression(
                            engine,
                            condition,
                            &layer,
                            matched_scope,
                            &mut plan.subqueries,
                        )?;
                    }
                    for assignment in assignments.iter_mut() {
                        rewrite_target_expression(
                            engine,
                            &mut assignment.value,
                            &layer,
                            matched_scope,
                            &mut plan.subqueries,
                        )?;
                        assignment.column =
                            writable_column(&layer, &assignment.column, "MERGE INTO")?;
                    }
                    validate_mapped_columns(
                        &assignments
                            .iter()
                            .map(|assignment| assignment.column.clone())
                            .collect::<Vec<_>>(),
                        duplicate_assignment,
                    )?;
                }
                MergeWhenPlan::DeleteMatched { condition }
                | MergeWhenPlan::NothingMatched { condition } => {
                    if let Some(condition) = condition {
                        rewrite_target_expression(
                            engine,
                            condition,
                            &layer,
                            matched_scope,
                            &mut plan.subqueries,
                        )?;
                    }
                }
                MergeWhenPlan::UpdateNotMatchedBySource {
                    condition,
                    assignments,
                } => {
                    if let Some(condition) = condition {
                        rewrite_target_expression(
                            engine,
                            condition,
                            &layer,
                            target_only_scope,
                            &mut plan.subqueries,
                        )?;
                    }
                    for assignment in assignments.iter_mut() {
                        rewrite_target_expression(
                            engine,
                            &mut assignment.value,
                            &layer,
                            target_only_scope,
                            &mut plan.subqueries,
                        )?;
                        assignment.column =
                            writable_column(&layer, &assignment.column, "MERGE INTO")?;
                    }
                    validate_mapped_columns(
                        &assignments
                            .iter()
                            .map(|assignment| assignment.column.clone())
                            .collect::<Vec<_>>(),
                        duplicate_assignment,
                    )?;
                }
                MergeWhenPlan::DeleteNotMatchedBySource { condition }
                | MergeWhenPlan::NothingNotMatchedBySource { condition } => {
                    if let Some(condition) = condition {
                        rewrite_target_expression(
                            engine,
                            condition,
                            &layer,
                            target_only_scope,
                            &mut plan.subqueries,
                        )?;
                    }
                }
                MergeWhenPlan::InsertNotMatched {
                    columns, values, ..
                } => {
                    let supplied_columns = if columns.is_empty() {
                        layer
                            .columns
                            .iter()
                            .take(values.len())
                            .map(|column| column.name.clone())
                            .collect::<Vec<_>>()
                    } else {
                        columns.clone()
                    };
                    *columns = supplied_columns
                        .iter()
                        .map(|column| writable_column(&layer, column, "MERGE INTO"))
                        .collect::<Result<Vec<_>, SQLError>>()?;
                    validate_mapped_columns(columns, duplicate_insert_column)?;
                }
                MergeWhenPlan::NothingNotMatched { .. } => {}
            }
        }
        rewrite_existing_view_checks(
            engine,
            &mut plan.view_checks,
            &layer,
            &plan.target_qualifier,
            &mut plan.subqueries,
        )?;
        let (returning, boundaries) = rewrite_merge_returning(
            engine,
            plan.returning,
            &layer,
            &plan.target_qualifier,
            &plan.returning_aliases,
            &source_schema,
            &mut plan.subqueries,
        )?;
        plan.returning = returning;
        if visited.len() == 1 {
            source_star_boundaries = boundaries;
        }
        plan.target_predicate = combine_view_predicate(
            engine,
            plan.target_predicate,
            &layer,
            &plan.target_qualifier,
            &mut plan.subqueries,
        )?;
        add_check_option(
            engine,
            &mut plan.view_checks,
            &layer,
            &plan.target_qualifier,
            &mut cascaded,
            &mut plan.subqueries,
        )?;
        plan.target = layer.source_name;
        plan.include_descendants = layer.source_include_descendants;
        if !super::view_triggers::target_is_view(engine, &plan.target)? {
            break;
        }
    }
    plan.returning = finalize_source_returning(
        engine,
        &plan.target,
        plan.returning,
        Some(&source_schema),
        &source_star_boundaries,
    )?;
    Ok(plan)
}

fn view_updatability_inner(
    engine: &Engine,
    name: &str,
    visited: &mut BTreeSet<String>,
) -> Result<ViewUpdatability, SQLError> {
    let definition = engine
        .view_definition(name)?
        .ok_or_else(|| SQLError::UnknownTable(name.to_string()))?;
    let schema = engine.stored_view_schema(&definition)?;
    let width = schema.len();
    let check_option = ViewCheckOption::from_options(&definition.options)
        .catalog_value()
        .to_string();
    let mut automatic = ViewMutationCapabilities::default();
    let mut automatic_insert_columns = vec![false; width];
    let mut automatic_columns = vec![false; width];
    let mut projected_catalog = ViewMutationCapabilities::default();
    let mut projected_catalog_insert_columns = vec![false; width];
    let mut projected_catalog_columns = vec![false; width];
    if let Some(layer) = automatic_view_layer(engine, name)? {
        if visited.insert(layer.canonical_name.clone()) {
            if engine.view_definition(&layer.source_name)?.is_some() {
                let source = view_updatability_inner(engine, &layer.source_name, visited)?;
                let source_columns = relation_columns(engine, &layer.source_name)?;
                let mapped = |source_capabilities: &[bool]| {
                    layer
                        .columns
                        .iter()
                        .map(|column| {
                            column.writable_source_column.as_ref().is_some_and(|name| {
                                source_columns
                                    .iter()
                                    .position(|candidate| candidate == name)
                                    .is_some_and(|position| {
                                        source_capabilities.get(position) == Some(&true)
                                    })
                            })
                        })
                        .collect::<Vec<_>>()
                };
                automatic_insert_columns = mapped(&source.runtime_insert_columns);
                automatic_columns = mapped(&source.runtime_columns);
                automatic = ViewMutationCapabilities {
                    insertable: source.runtime.insertable
                        && automatic_insert_columns.iter().any(|value| *value),
                    updatable: source.runtime.updatable
                        && automatic_columns.iter().any(|value| *value),
                    deletable: source.runtime.deletable,
                };
                projected_catalog_insert_columns = mapped(&source.catalog_insert_columns);
                projected_catalog_columns = mapped(&source.catalog_columns);
                projected_catalog = ViewMutationCapabilities {
                    insertable: source.catalog.insertable
                        && projected_catalog_insert_columns.iter().any(|value| *value),
                    updatable: source.catalog.updatable
                        && projected_catalog_columns.iter().any(|value| *value),
                    deletable: source.catalog.deletable,
                };
            } else {
                automatic_insert_columns = layer
                    .columns
                    .iter()
                    .map(|column| column.writable_source_column.is_some())
                    .collect();
                automatic_columns.clone_from(&automatic_insert_columns);
                automatic = ViewMutationCapabilities {
                    insertable: automatic_insert_columns.iter().any(|value| *value),
                    updatable: automatic_columns.iter().any(|value| *value),
                    deletable: true,
                };
                projected_catalog = automatic;
                projected_catalog_insert_columns.clone_from(&automatic_insert_columns);
                projected_catalog_columns.clone_from(&automatic_columns);
            }
            visited.remove(&layer.canonical_name);
        }
    }
    let active_insert =
        active_unconditional_instead_rule(engine, name, uqa_sql::ast::RuleEvent::Insert)?;
    let active_update =
        active_unconditional_instead_rule(engine, name, uqa_sql::ast::RuleEvent::Update)?;
    let active_delete =
        active_unconditional_instead_rule(engine, name, uqa_sql::ast::RuleEvent::Delete)?;
    let runtime = ViewMutationCapabilities {
        insertable: automatic.insertable || active_insert,
        updatable: automatic.updatable || active_update,
        deletable: automatic.deletable || active_delete,
    };
    let runtime_insert_columns = automatic_insert_columns
        .iter()
        .map(|column| *column || active_insert)
        .collect();
    let runtime_columns = automatic_columns
        .iter()
        .map(|column| *column || active_update)
        .collect();
    let rule_insertable =
        has_unconditional_instead_rule(engine, name, uqa_sql::ast::RuleEvent::Insert)?;
    let rule_updatable =
        has_unconditional_instead_rule(engine, name, uqa_sql::ast::RuleEvent::Update)?;
    let rule_deletable =
        has_unconditional_instead_rule(engine, name, uqa_sql::ast::RuleEvent::Delete)?;
    let catalog = ViewMutationCapabilities {
        insertable: projected_catalog.insertable || rule_insertable,
        updatable: projected_catalog.updatable || rule_updatable,
        deletable: projected_catalog.deletable || rule_deletable,
    };
    let catalog_insert_columns = projected_catalog_insert_columns
        .iter()
        .map(|column| *column || rule_insertable)
        .collect();
    let catalog_columns = if catalog.fully_updatable() {
        projected_catalog_columns
            .iter()
            .map(|column| *column || rule_updatable)
            .collect()
    } else {
        vec![false; width]
    };
    Ok(ViewUpdatability {
        automatic,
        runtime,
        runtime_insert_columns,
        runtime_columns,
        catalog,
        catalog_insert_columns,
        catalog_columns,
        check_option,
    })
}

pub(in crate::sql) fn view_updatability(
    engine: &Engine,
    name: &str,
) -> Result<ViewUpdatability, SQLError> {
    view_updatability_inner(engine, name, &mut BTreeSet::new())
}

fn active_unconditional_instead_rule(
    engine: &Engine,
    relation: &str,
    event: uqa_sql::ast::RuleEvent,
) -> Result<bool, SQLError> {
    Ok(engine
        .rules_for(relation, event)?
        .iter()
        .any(|rule| rule.definition.instead && rule.definition.condition.is_none()))
}

fn has_unconditional_instead_rule(
    engine: &Engine,
    relation: &str,
    event: uqa_sql::ast::RuleEvent,
) -> Result<bool, SQLError> {
    Ok(engine
        .rule_definitions_for(relation, event)?
        .iter()
        .any(|rule| rule.definition.instead && rule.definition.condition.is_none()))
}

pub(in crate::sql) fn validate_view_definition_check_option(
    engine: &Engine,
    name: &str,
    definition: &StoredView,
) -> Result<(), SQLError> {
    if ViewCheckOption::from_options(&definition.options) == ViewCheckOption::None {
        return Ok(());
    }
    let updatable =
        automatic_view_layer_from_definition(engine, name, definition)?.is_some_and(|layer| {
            layer
                .columns
                .iter()
                .any(|column| column.writable_source_column.is_some())
        });
    if updatable {
        return Ok(());
    }
    Err(SQLError::Routine {
        sqlstate: "0A000".into(),
        message: "WITH CHECK OPTION is supported only on automatically updatable views".into(),
    })
}
