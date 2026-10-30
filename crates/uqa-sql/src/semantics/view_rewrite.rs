//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` 18 automatically updatable view analysis and DML rewriting.

use std::collections::BTreeSet;

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

mod captured_projection;
mod columns;
pub use columns::relation_columns_with_control;
mod correlation;
pub use captured_projection::{capture_view_projection, CapturedViewProjection};
pub use captured_projection::{capture_view_projection_owned, OwnedCapturedViewProjection};
mod deferred;
pub use deferred::{
    bind_outer_rewrite_scope, bind_outer_rewrite_scope_with_control,
    resolve_outer_expression_rewrites, resolve_outer_rewrites, resolve_outer_rewrites_owned,
    resolve_outer_source_rewrites, resolve_outer_sources, resolve_outer_sources_owned,
    DeferredOuterRewrite,
};
mod layer_privileges;
pub mod production;
mod returning;
mod rewrite_insert;
mod rewrite_merge;
mod rewrite_update_delete;
pub mod rule_inputs;
mod updatability;
mod validation;

use correlation::{
    collect_expression_subquery_ids, delete_ordinary_subquery_ids, dml_analysis_scope,
    dml_source_schema, insert_conflict_subquery_ids, insert_input_width,
    merge_matched_subquery_ids, merge_target_only_subquery_ids, returning_subquery_ids,
    rewrite_correlated_dml_context, schema_public_columns, update_ordinary_subquery_ids,
    validate_delete_expressions, validate_insert_expressions, validate_merge_expressions,
    validate_update_expressions, CorrelatedDmlContext,
};
use layer_privileges::LayerPrivileges;
use returning::{
    add_check_option, bind_unqualified_source_positions, combine_view_predicate, dml_target_width,
    finalize_source_returning, instead_of_trigger_definition, preserve_view_rule_returning,
    record_view_rule_relation, retarget_source_expression, rewrite_existing_view_checks,
    rewrite_merge_returning, rewrite_returning, rewrite_target_expression,
};
pub use rewrite_insert::rewrite_insert_to_base;
pub use rewrite_merge::rewrite_merge_to_base;
pub use rewrite_update_delete::{
    rewrite_delete_rule_inputs, rewrite_delete_to_base, rewrite_update_rule_inputs,
    rewrite_update_to_base,
};
pub use rule_inputs::{rule_input_requirements, validate_grouped_rule_inputs};
pub use updatability::NotUpdatableReason;
use updatability::{view_not_updatable, ColumnRestriction, ColumnWrite, ViewCommand};
use validation::{
    duplicate_assignment, duplicate_insert_column, layer_column, validate_direct_view_rule_path,
    validate_insert_targets, validate_mapped_columns, validate_merge_targets,
    validate_public_delete_contract, validate_public_insert_contract,
    validate_public_update_contract, validate_public_view_targets, validate_update_targets,
    validate_view_expression, validate_writable_columns, writable_column, ExpressionScope,
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
    pub collation_oid: Option<u32>,
    pub expression: ScalarExpr,
    pub writable_source_column: Option<String>,
}

#[derive(Debug, Clone)]
pub struct AutomaticViewLayer {
    pub canonical_name: String,
    pub source_name: String,
    pub source_qualifier: String,
    pub source_column_map: Vec<(String, String)>,
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
            .binary_search_by(|(name, _)| name.as_str().cmp(column))
            .map_or(column, |index| self.source_column_map[index].1.as_str())
    }

    /// Whether a column of the view is a column of its base relation, which INSERT and UPDATE need and DELETE does not.
    pub fn has_writable_column(&self) -> bool {
        self.columns
            .iter()
            .any(|column| column.writable_source_column.is_some())
    }

    /// Why `column`, which is not a column of the base relation, cannot be written, as `view_col_is_auto_updatable` reports it.
    fn column_restriction(&self, column: &ViewColumn) -> ColumnRestriction {
        let name = match &column.expression {
            ScalarExpr::Column(name) => Some(name),
            ScalarExpr::QualifiedColumn { qualifier, column }
                if source_qualifier_matches(
                    qualifier,
                    &self.source_qualifier,
                    &self.source_name,
                ) =>
            {
                Some(column)
            }
            _ => None,
        };
        if name.is_some_and(|name| {
            crate::schema::columns::POSTGRES_SYSTEM_COLUMNS.contains(&name.as_str())
        }) {
            ColumnRestriction::SystemColumn
        } else {
            ColumnRestriction::Computed
        }
    }
}

/// What `view_query_is_auto_updatable` finds in a view's own query, short of its test for an updatable column, which depends on the command.
#[derive(Debug)]
pub enum ViewQueryShape {
    Updatable(Box<AutomaticViewLayer>),
    NotUpdatable(NotUpdatableReason),
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

pub fn relation_columns(
    services: ViewRewriteContext<'_>,
    relation: &str,
) -> Result<Vec<String>, SQLError> {
    Ok(relation_columns_with_control(
        services,
        relation,
        &uqa_core::memory::ProductionControl::uncontrolled(),
    )?
    .into_uncontrolled()
    .expect("ordinary view source columns"))
}

fn source_qualifier_matches(qualifier: &str, source_qualifier: &str, source_name: &str) -> bool {
    qualifier == source_qualifier
        || qualifier == source_name
        || RelationIdentity::borrowed_reference(source_name)
            .is_some_and(|identity| identity.name == uqa_core::RelationName::decoded(qualifier))
}

pub fn automatic_view_layer(
    services: ViewRewriteContext<'_>,
    name: &str,
) -> Result<Option<AutomaticViewLayer>, SQLError> {
    Ok(match view_query_shape(services, name)? {
        Some(ViewQueryShape::Updatable(layer)) => Some(*layer),
        Some(ViewQueryShape::NotUpdatable(_)) | None => None,
    })
}

/// The shape of the query of view `name`, or `None` when `name` names no regular view.
pub fn view_query_shape(
    services: ViewRewriteContext<'_>,
    name: &str,
) -> Result<Option<ViewQueryShape>, SQLError> {
    let Some(canonical_name) = services
        .catalog
        .try_resolve_view_name(name)
        .map_err(|error| SQLError::Internal(format!("resolve DML view `{name}`: {error}")))?
    else {
        return Ok(None);
    };
    let control = uqa_core::memory::ProductionControl::uncontrolled();
    let mut shape = None;
    services.catalog.with_view_definition_with_control(
        &canonical_name,
        &control,
        &mut |definition| {
            let definition = definition.ok_or_else(|| SQLError::UnknownTable(name.to_string()))?;
            if definition.kind == StoredViewKind::View {
                shape = Some(
                    production::shape_from_definition(
                        services,
                        &canonical_name,
                        definition,
                        &control,
                    )?
                    .finish(&control)?
                    .into_uncontrolled()
                    .expect("ordinary view shape"),
                );
            }
            Ok(())
        },
    )?;
    Ok(shape)
}

/// The catalog name of view `name`.
fn canonical_view_name(services: ViewRewriteContext<'_>, name: &str) -> Result<String, SQLError> {
    services
        .catalog
        .try_resolve_view_name(name)
        .map_err(|error| SQLError::Internal(format!("resolve DML view `{name}`: {error}")))?
        .ok_or_else(|| SQLError::UnknownTable(name.to_string()))
}

/// The layer of view `view` that `command` is rewritten through, or the error `rewriteTargetView` reports when the view's query does not allow the rewrite.
fn rewritable_layer(
    services: ViewRewriteContext<'_>,
    view: &str,
    command: ViewCommand,
) -> Result<AutomaticViewLayer, SQLError> {
    match view_query_shape(services, view)? {
        Some(ViewQueryShape::Updatable(layer)) => Ok(*layer),
        Some(ViewQueryShape::NotUpdatable(reason)) => {
            Err(view_not_updatable(view, command, reason))
        }
        None => Err(SQLError::UnknownTable(view.to_string())),
    }
}

/// The next layer the rewrite of `command` passes through at view `view`: the first layer, checked already, while `initial` holds it; `None` where an unconditional INSTEAD rule of a view that cannot be rewritten handles the command. As `RewriteQuery` does, conditional INSTEAD rules without an unconditional one are rejected before the view's query is examined.
fn next_rewritten_layer(
    services: ViewRewriteContext<'_>,
    view: &str,
    initial: &mut Option<AutomaticViewLayer>,
    rewrite_suppressed: bool,
    event: crate::ast::RuleEvent,
    command: ViewCommand,
) -> Result<Option<AutomaticViewLayer>, SQLError> {
    if let Some(layer) = initial.take() {
        return Ok(Some(layer));
    }
    if !rewrite_suppressed {
        validate_direct_view_rule_path(services, view, event, command)?;
    }
    match view_query_shape(services, view)? {
        Some(ViewQueryShape::Updatable(layer)) => Ok(Some(*layer)),
        shape => {
            if active_unconditional_instead_rule(services, view, event)? {
                return Ok(None);
            }
            Err(match shape {
                Some(ViewQueryShape::NotUpdatable(reason)) => {
                    view_not_updatable(view, command, reason)
                }
                _ => SQLError::UnknownTable(view.to_string()),
            })
        }
    }
}

fn view_query_shape_from_definition(
    services: ViewRewriteContext<'_>,
    canonical_name: &str,
    definition: &StoredView,
) -> Result<ViewQueryShape, SQLError> {
    let control = uqa_core::memory::ProductionControl::uncontrolled();
    Ok(
        production::shape_from_definition(services, canonical_name, definition.into(), &control)?
            .finish(&control)?
            .into_uncontrolled()
            .expect("ordinary view shape"),
    )
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
    services
        .catalog
        .capture_rules(relation, event)?
        .any(|rule| rule.definition.instead && rule.definition.condition.is_none())
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
    // `DefineView` reports why the view is not automatically updatable as the hint.
    let reason = match view_query_shape_from_definition(services, name, definition)? {
        ViewQueryShape::Updatable(layer) if layer.has_writable_column() => return Ok(()),
        ViewQueryShape::Updatable(_) => NotUpdatableReason::NoUpdatableColumns,
        ViewQueryShape::NotUpdatable(reason) => reason,
    };
    Err(SQLError::Diagnostic {
        sqlstate: "0A000".into(),
        message: "WITH CHECK OPTION is supported only on automatically updatable views".into(),
        detail: None,
        hint: Some(reason.detail().into()),
    })
}
