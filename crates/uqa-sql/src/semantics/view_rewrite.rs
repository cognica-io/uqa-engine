//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` 18 automatically updatable view analysis and DML rewriting.

use std::collections::{BTreeMap, BTreeSet};

use crate::ast::{ReturningAliases, TriggerEvent, TriggerTiming};
use crate::plan::{
    AssignmentPlan, ComputePlan, ConflictActionPlan, ConflictPlan, DeletePlan, InsertPlan,
    MergePlan, MergeWhenPlan, ProjectionPlan, QueryPlan, RelationalPlan, SourcePlan, UpdatePlan,
    ViewCheckPlan, ViewRuleInsertPlan, ViewRuleReturningPlan, ViewRuleUpdatePlan,
};
use crate::SQLError;
use crate::{ColumnIdentity, RowSchema, ScalarExpr};

use crate::{
    catalog::view::{StoredViewKind, ViewRewriteDefinition as StoredView},
    RelationIdentity,
};

use crate::binding::snapshot::BindingSnapshot as CteScope;
pub mod context;
use context::{stored_view_schema, ViewRewriteContext};

mod correlation;
mod returning;
mod rewrite_insert;
mod rewrite_merge;
mod rewrite_update_delete;
pub mod rule_inputs;
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
};
pub use rewrite_insert::rewrite_insert_to_base;
pub use rewrite_merge::rewrite_merge_to_base;
pub use rewrite_update_delete::{rewrite_delete_to_base, rewrite_update_to_base};
pub use rule_inputs::rule_input_requirements;
use validation::{
    duplicate_assignment, duplicate_insert_column, layer_column, merge_action_capability_error,
    validate_direct_view_rule_path, validate_insert_targets, validate_mapped_columns,
    validate_merge_targets, validate_public_delete_contract, validate_public_insert_contract,
    validate_public_update_contract, validate_public_view_targets, validate_update_targets,
    validate_view_expression, writable_column, ExpressionScope,
};
pub use validation::{merge_view_target_path, MergeViewTargetPath};
pub use validation::{validate_public_merge_contract, validate_public_merge_targets};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewCheckOption {
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
pub struct ViewColumn {
    pub name: String,
    pub expression: ScalarExpr,
    pub writable_source_column: Option<String>,
}

#[derive(Debug, Clone)]
pub struct AutomaticViewLayer {
    pub canonical_name: String,
    pub source_name: String,
    pub source_qualifier: String,
    pub source_column_map: BTreeMap<String, String>,
    pub source_include_descendants: bool,
    pub source_schema: RowSchema,
    pub columns: Vec<ViewColumn>,
    pub predicate: Option<ScalarExpr>,
    pub subqueries: Vec<QueryPlan>,
    pub check_option: ViewCheckOption,
}

impl AutomaticViewLayer {
    pub fn physical_source_column<'a>(&'a self, column: &'a str) -> &'a str {
        self.source_column_map
            .get(column)
            .map_or(column, String::as_str)
    }
}

pub use crate::catalog::view::ViewMutationCapabilities;

#[derive(Debug, Clone)]
pub struct ViewUpdatability {
    pub automatic: ViewMutationCapabilities,
    runtime: ViewMutationCapabilities,
    runtime_insert_columns: Vec<bool>,
    runtime_columns: Vec<bool>,
    pub catalog: ViewMutationCapabilities,
    catalog_insert_columns: Vec<bool>,
    pub catalog_columns: Vec<bool>,
    pub check_option: String,
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

pub fn relation_columns(
    services: ViewRewriteContext<'_>,
    relation: &str,
) -> Result<Vec<String>, SQLError> {
    if let Some(view) = services.catalog.view_definition(relation)? {
        let schema = stored_view_schema(services, &view)?;
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
    let definitions = services
        .catalog
        .try_describe_table(relation)
        .map_err(|error| SQLError::Internal(format!("describe view source `{relation}`: {error}")))?
        .ok_or_else(|| SQLError::UnknownTable(relation.to_string()))?;
    if definitions.is_empty() {
        return services
            .catalog
            .try_table_columns(relation)
            .map_err(|error| {
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
    source_columns: &BTreeMap<String, String>,
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
    source_columns.get(&column).cloned()
}

pub fn automatic_view_layer(
    services: ViewRewriteContext<'_>,
    name: &str,
) -> Result<Option<AutomaticViewLayer>, SQLError> {
    let Some(canonical_name) = services
        .catalog
        .try_resolve_view_name(name)
        .map_err(|error| SQLError::Internal(format!("resolve DML view `{name}`: {error}")))?
    else {
        return Ok(None);
    };
    let definition = services
        .catalog
        .view_definition(&canonical_name)?
        .ok_or_else(|| SQLError::UnknownTable(name.to_string()))?;
    automatic_view_layer_from_definition(services, &canonical_name, &definition)
}

#[expect(
    clippy::too_many_lines,
    reason = "preserves view qualifier and row identity"
)]
fn automatic_view_layer_from_definition(
    services: ViewRewriteContext<'_>,
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
            column_aliases,
            include_descendants,
            ..
        },
    ) = block.from.as_ref()
    else {
        return Ok(None);
    };
    let analysis_scope = services.catalog.binding_scope()?;
    let source_schema = crate::semantics::view_rewrite::context::analyze_source_plan_schema(
        services,
        source_plan,
        &[],
        &analysis_scope,
        None,
    )?;
    let resolver = crate::binding::scoped_types::BindingTypeResolver {
        routines: services.catalog,
        scope: &analysis_scope,
    };
    for projection in &block.projections {
        if crate::semantics::sets::validation::expression_may_return_set(
            services.catalog,
            &resolver,
            &projection.expr,
            &source_schema,
            &[],
        )? {
            return Ok(None);
        }
    }
    let source_qualifier = alias.as_deref().unwrap_or(qualifier).to_string();
    let source_columns = relation_columns(services, source_name)?;
    let mut visible_source_columns = source_columns.clone();
    for (column, alias) in visible_source_columns.iter_mut().zip(column_aliases) {
        column.clone_from(alias);
    }
    let source_column_map = visible_source_columns
        .iter()
        .cloned()
        .zip(source_columns.iter().cloned())
        .collect::<BTreeMap<_, _>>();
    let mut expressions = Vec::new();
    for projection in &block.projections {
        match &projection.expr {
            ScalarExpr::Star => {
                expressions.extend(
                    visible_source_columns
                        .iter()
                        .cloned()
                        .map(ScalarExpr::Column),
                );
            }
            ScalarExpr::QualifiedStar(star_qualifier)
                if source_qualifier_matches(star_qualifier, &source_qualifier, source_name) =>
            {
                expressions.extend(visible_source_columns.iter().cloned().map(|column| {
                    ScalarExpr::QualifiedColumn {
                        qualifier: source_qualifier.clone(),
                        column,
                    }
                }));
            }
            expression => expressions.push(expression.clone()),
        }
    }
    let schema = stored_view_schema(services, definition)?;
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
                &source_column_map,
            ),
            name,
            expression,
        })
        .collect();
    Ok(Some(AutomaticViewLayer {
        canonical_name: canonical_name.to_string(),
        source_name: source_name.clone(),
        source_qualifier,
        source_column_map,
        source_include_descendants: *include_descendants,
        source_schema,
        columns,
        predicate: block.r#where.clone(),
        subqueries: block.subqueries.clone(),
        check_option: ViewCheckOption::from_options(&definition.options),
    }))
}

mod layer_rewrite;
use layer_rewrite::embed_layer_expression;

pub fn has_instead_of_trigger(
    services: ViewRewriteContext<'_>,
    view: &str,
    event: TriggerEvent,
) -> Result<bool, SQLError> {
    let Some(canonical) = services
        .catalog
        .try_resolve_view_name(view)
        .map_err(|error| SQLError::Internal(format!("resolve DML view `{view}`: {error}")))?
    else {
        return Ok(false);
    };
    instead_of_trigger_definition(services, &canonical, event)
}

#[expect(
    clippy::too_many_lines,
    reason = "preserves view qualifier and row identity"
)]
fn view_updatability_inner(
    services: ViewRewriteContext<'_>,
    name: &str,
    visited: &mut BTreeSet<String>,
) -> Result<ViewUpdatability, SQLError> {
    let definition = services
        .catalog
        .view_definition(name)?
        .ok_or_else(|| SQLError::UnknownTable(name.to_string()))?;
    let schema = stored_view_schema(services, &definition)?;
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
    if let Some(layer) = automatic_view_layer(services, name)? {
        if visited.insert(layer.canonical_name.clone()) {
            if services
                .catalog
                .view_definition(&layer.source_name)?
                .is_some()
            {
                let source = view_updatability_inner(services, &layer.source_name, visited)?;
                let source_columns = relation_columns(services, &layer.source_name)?;
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
        active_unconditional_instead_rule(services, name, crate::ast::RuleEvent::Insert)?;
    let active_update =
        active_unconditional_instead_rule(services, name, crate::ast::RuleEvent::Update)?;
    let active_delete =
        active_unconditional_instead_rule(services, name, crate::ast::RuleEvent::Delete)?;
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
        has_unconditional_instead_rule(services, name, crate::ast::RuleEvent::Insert)?;
    let rule_updatable =
        has_unconditional_instead_rule(services, name, crate::ast::RuleEvent::Update)?;
    let rule_deletable =
        has_unconditional_instead_rule(services, name, crate::ast::RuleEvent::Delete)?;
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

pub fn view_updatability(
    services: ViewRewriteContext<'_>,
    name: &str,
) -> Result<ViewUpdatability, SQLError> {
    view_updatability_inner(services, name, &mut BTreeSet::new())
}

fn active_unconditional_instead_rule(
    services: ViewRewriteContext<'_>,
    relation: &str,
    event: crate::ast::RuleEvent,
) -> Result<bool, SQLError> {
    Ok(services
        .catalog
        .rules_for(relation, event)?
        .iter()
        .any(|rule| rule.definition.instead && rule.definition.condition.is_none()))
}

fn has_unconditional_instead_rule(
    services: ViewRewriteContext<'_>,
    relation: &str,
    event: crate::ast::RuleEvent,
) -> Result<bool, SQLError> {
    Ok(services
        .catalog
        .rule_definitions_for(relation, event)?
        .iter()
        .any(|rule| rule.definition.instead && rule.definition.condition.is_none()))
}

pub fn validate_view_definition_check_option(
    services: ViewRewriteContext<'_>,
    name: &str,
    definition: &StoredView,
) -> Result<(), SQLError> {
    if ViewCheckOption::from_options(&definition.options) == ViewCheckOption::None {
        return Ok(());
    }
    let updatable =
        automatic_view_layer_from_definition(services, name, definition)?.is_some_and(|layer| {
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
