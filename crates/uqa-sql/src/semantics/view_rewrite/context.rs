//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Catalog and authorization inputs for automatic view analysis and DML rewriting.

use crate::{
    ast::{ColumnDef, RuleEvent, TriggerEvent, TriggerTiming},
    binding::snapshot::BindingSnapshot,
    catalog::{
        analysis::CatalogReadView,
        events::StoredRule,
        resolution::RelationNameResolution,
        view::{StoredViewKind, ViewRewriteDefinition},
    },
    plan::{QueryPlan, SourcePlan},
    semantics::sets::SetFunctionCatalog,
    RowSchema, SQLError, SQLParam,
};
use std::collections::BTreeSet;

pub trait ViewRewriteCatalog: SetFunctionCatalog {
    fn view_definition(&self, name: &str) -> Result<Option<ViewRewriteDefinition>, SQLError>;
    fn try_resolve_view_name(&self, name: &str) -> Result<Option<String>, String>;
    fn try_describe_table(&self, name: &str) -> Result<Option<Vec<ColumnDef>>, String>;
    fn try_table_columns(&self, name: &str) -> Result<Vec<String>, String>;
    fn rules_for(&self, name: &str, event: RuleEvent) -> Result<Vec<StoredRule>, SQLError>;
    fn rule_definitions_for(
        &self,
        name: &str,
        event: RuleEvent,
    ) -> Result<Vec<StoredRule>, SQLError>;
    fn has_trigger_definition(
        &self,
        name: &str,
        timing: TriggerTiming,
        event: TriggerEvent,
        row: bool,
    ) -> Result<bool, SQLError>;
    fn rule_new_row_columns(&self, rule: &StoredRule)
        -> Result<Option<BTreeSet<String>>, SQLError>;
    fn target_view_kind(&self, name: &str) -> Result<Option<StoredViewKind>, SQLError>;
    fn binding_scope(&self) -> Result<BindingSnapshot, SQLError>;
    fn restored_view_catalog(&self) -> (CatalogReadView, RelationNameResolution);
}

#[derive(Clone, Copy)]
pub struct ViewRewriteContext<'a> {
    pub catalog: &'a dyn ViewRewriteCatalog,
    pub authorization: &'a dyn super::super::view_privileges::ViewPrivilegeCatalog,
}

pub fn stored_view_schema(
    services: ViewRewriteContext<'_>,
    definition: &ViewRewriteDefinition,
) -> Result<RowSchema, SQLError> {
    let (catalog, resolution) = services.catalog.restored_view_catalog();
    definition.row_schema(services.catalog, catalog, resolution)
}

pub(super) fn analyze_source_plan_schema(
    services: ViewRewriteContext<'_>,
    source: &SourcePlan,
    params: &[SQLParam],
    scope: &BindingSnapshot,
    outer: Option<&RowSchema>,
) -> Result<RowSchema, SQLError> {
    crate::binding::analyze_source_plan_schema(
        services.catalog,
        source,
        params,
        &scope.context(),
        outer,
    )
}
pub(super) fn analyze_query_plan_schema(
    services: ViewRewriteContext<'_>,
    query: &QueryPlan,
    params: &[SQLParam],
    scope: &BindingSnapshot,
    outer: Option<&RowSchema>,
) -> Result<RowSchema, SQLError> {
    crate::binding::analyze_query_plan_schema(
        services.catalog,
        query,
        params,
        &scope.context(),
        outer,
    )
}
pub(super) fn target_is_view(
    services: ViewRewriteContext<'_>,
    name: &str,
) -> Result<bool, SQLError> {
    Ok(services.catalog.target_view_kind(name)?.is_some())
}

pub(super) fn relation_suppresses_original_query(
    services: ViewRewriteContext<'_>,
    table: &str,
    event: RuleEvent,
) -> Result<bool, SQLError> {
    Ok(services
        .catalog
        .rules_for(table, event)?
        .iter()
        .any(|rule| rule.definition.instead && rule.definition.condition.is_none()))
}
pub(super) fn relation_has_returning_provider(
    services: ViewRewriteContext<'_>,
    table: &str,
    event: RuleEvent,
) -> Result<bool, SQLError> {
    Ok(services
        .catalog
        .rules_for(table, event)?
        .iter()
        .any(|rule| {
            rule.definition
                .actions
                .iter()
                .any(crate::semantics::rules::statement_has_returning)
        }))
}
