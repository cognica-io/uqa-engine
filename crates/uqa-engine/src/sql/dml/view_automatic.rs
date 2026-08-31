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

mod correlation;
mod returning;
mod rewrite_insert;
mod rewrite_merge;
mod rewrite_update_delete;
mod validation;

use correlation::{
    collect_expression_subquery_ids, delete_ordinary_subquery_ids, dml_analysis_scope,
    dml_source_schema, insert_conflict_subquery_ids, insert_input_width,
    merge_matched_subquery_ids, merge_target_only_subquery_ids, returning_subquery_ids,
    rewrite_correlated_dml_context, schema_public_columns, update_ordinary_subquery_ids,
    validate_delete_expressions, validate_insert_expressions, validate_merge_expressions,
    validate_update_expressions, CorrelatedDmlContext,
};
use returning::{
    add_check_option, bind_unqualified_source_positions, combine_view_predicate, dml_target_width,
    finalize_source_returning, instead_of_trigger_definition, preserve_view_rule_returning,
    record_view_rule_relation, retarget_source_expression, rewrite_existing_view_checks,
    rewrite_merge_returning, rewrite_returning, rewrite_target_expression,
    stored_view_document_row,
};
pub(super) use rewrite_insert::rewrite_insert_to_base;
pub(super) use rewrite_merge::rewrite_merge_to_base;
pub(super) use rewrite_update_delete::{rewrite_delete_to_base, rewrite_update_to_base};
use validation::{
    duplicate_assignment, duplicate_insert_column, layer_column, merge_action_capability_error,
    validate_direct_view_rule_path, validate_insert_targets, validate_mapped_columns,
    validate_merge_targets, validate_public_delete_contract, validate_public_insert_contract,
    validate_public_update_contract, validate_public_view_targets, validate_update_targets,
    validate_view_expression, writable_column, ExpressionScope,
};
pub(super) use validation::{merge_view_target_path, MergeViewTargetPath};
pub(in crate::sql) use validation::{
    validate_public_merge_contract, validate_public_merge_targets,
};

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
    let analysis_scope = CteScope::new_for_current_routine(engine);
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
    let scope = CteScope::new_for_current_routine(engine);
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
