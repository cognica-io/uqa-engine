//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Supply view analysis with the immutable catalog already selected by its caller.

use crate::catalog::{CatalogReadView, RelationNameResolution};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};
use uqa_core::RelationIdentity;
use uqa_sql::{
    ast::{ColumnDef, RuleEvent, TriggerEvent, TriggerTiming},
    binding::snapshot::BindingSnapshot,
    catalog::{
        events::{selection, StoredRule},
        view::{StoredViewKind, ViewRewriteDefinition},
    },
    semantics::view_rewrite::context::ViewRewriteCatalog,
    SQLError,
};

pub struct RetainedViewCatalog<'a> {
    pub catalog: &'a CatalogReadView,
    pub resolution: &'a RelationNameResolution,
    pub fallback: &'a dyn ViewRewriteCatalog,
    pub replica: bool,
}

impl RetainedViewCatalog<'_> {
    fn relation(&self, name: &str) -> Result<RelationIdentity, SQLError> {
        let (name, _) = self
            .catalog
            .relation_kind_resolution(self.resolution, name)?
            .into_found()
            .ok_or_else(|| SQLError::UnknownTable(name.into()))?;
        RelationIdentity::from_legacy_name(&name).map_err(SQLError::Internal)
    }
}

impl ViewRewriteCatalog for RetainedViewCatalog<'_> {
    fn view_definition(&self, name: &str) -> Result<Option<ViewRewriteDefinition>, SQLError> {
        Ok(self
            .catalog
            .view_resolved(self.resolution, name)?
            .map(super::StoredView::rewrite_definition))
    }
    fn try_resolve_view_name(&self, name: &str) -> Result<Option<String>, String> {
        self.catalog
            .view_name_resolved(self.resolution, name)
            .map_err(|error| error.to_string())
    }
    fn try_describe_table(&self, name: &str) -> Result<Option<Vec<ColumnDef>>, String> {
        if let Some(table) = self
            .catalog
            .table_resolved(self.resolution, name)
            .map_err(|error| error.to_string())?
        {
            return Ok(Some(table.columns.as_ref().clone()));
        }
        Ok(self
            .catalog
            .foreign_table_resolved(self.resolution, name)
            .map_err(|error| error.to_string())?
            .map(|table| table.columns.clone()))
    }
    fn try_table_columns(&self, name: &str) -> Result<Vec<String>, String> {
        let columns = self.try_describe_table(name)?.unwrap_or_default();
        if columns.is_empty() {
            return self.fallback.try_table_columns(name);
        }
        Ok(columns.into_iter().map(|column| column.name).collect())
    }
    fn rules_for(&self, name: &str, event: RuleEvent) -> Result<Vec<StoredRule>, SQLError> {
        Ok(selection::active_rules(
            &self.catalog.snapshot().definitions.rules,
            &self.relation(name)?,
            event,
            self.replica,
        ))
    }
    fn rule_definitions_for(
        &self,
        name: &str,
        event: RuleEvent,
    ) -> Result<Vec<StoredRule>, SQLError> {
        Ok(selection::rule_definitions(
            &self.catalog.snapshot().definitions.rules,
            &self.relation(name)?,
            event,
        ))
    }
    fn has_trigger_definition(
        &self,
        name: &str,
        timing: TriggerTiming,
        event: TriggerEvent,
        row: bool,
    ) -> Result<bool, SQLError> {
        Ok(selection::has_trigger_definition(
            &self.catalog.snapshot().definitions.triggers,
            &self.relation(name)?,
            timing,
            event,
            row,
        ))
    }
    fn rule_new_row_columns(
        &self,
        rule: &StoredRule,
    ) -> Result<Option<BTreeSet<String>>, SQLError> {
        self.fallback.rule_new_row_columns(rule)
    }
    fn target_view_kind(&self, name: &str) -> Result<Option<StoredViewKind>, SQLError> {
        Ok(self
            .catalog
            .view_resolved(self.resolution, name)?
            .map(|view| view.kind))
    }
    fn binding_scope(&self) -> Result<BindingSnapshot, SQLError> {
        Ok(BindingSnapshot {
            catalog: Arc::new(self.catalog.clone()),
            resolution: self.resolution.clone(),
            ctes: BTreeMap::new(),
            deferred_ctes: BTreeMap::new(),
            non_returning_ctes: BTreeSet::new(),
            scalar_subqueries: Vec::new(),
        })
    }
    fn restored_view_catalog(
        &self,
    ) -> (
        uqa_sql::catalog::analysis::CatalogReadView,
        RelationNameResolution,
    ) {
        (Arc::new(self.catalog.clone()), self.resolution.clone())
    }
}
