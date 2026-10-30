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
    pub control: uqa_core::memory::OwnedProductionControl,
}

impl RetainedViewCatalog<'_> {
    fn relation_with_control(
        &self,
        name: &str,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<uqa_core::memory::Produced<RelationIdentity>, SQLError> {
        let resolved =
            self.catalog
                .relation_kind_resolution_with_control(self.resolution, name, control)?;
        let crate::catalog::RelationResolution::Found(canonical, _) = &*resolved else {
            return Err(SQLError::UnknownTable(
                control.copy_text(name)?.into_parts().0,
            ));
        };
        let reference = RelationIdentity::try_borrowed_reference(canonical).map_err(|error| {
            control
                .format(format_args!("{error}"))
                .map(|text| SQLError::Internal(text.into_parts().0))
                .unwrap_or_else(Into::into)
        })?;
        Ok(reference.identity_with_control(uqa_core::RelationName::decoded("public"), control)?)
    }
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
    fn capture_rules(
        &self,
        name: &str,
        event: RuleEvent,
    ) -> Result<selection::CapturedRules, SQLError> {
        self.capture_rules_with_control(name, event, &self.control.control())
    }
    fn capture_rules_with_control(
        &self,
        name: &str,
        event: RuleEvent,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<selection::CapturedRules, SQLError> {
        let relation = self.relation_with_control(name, control)?;
        selection::CapturedRules::from_catalog(
            Arc::clone(&self.catalog.snapshot().definitions.rules),
            relation,
            event,
            self.replica,
            control,
        )
    }
    fn try_table_column_names_with_control(
        &self,
        name: &str,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<uqa_core::memory::Produced<Vec<String>>, SQLError> {
        if let Some(columns) = self.try_relation_column_names_with_control(name, control)? {
            return Ok(columns);
        }
        self.fallback
            .try_table_column_names_with_control(name, control)
    }

    fn try_relation_column_names_with_control(
        &self,
        name: &str,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<Option<uqa_core::memory::Produced<Vec<String>>>, SQLError> {
        control.check()?;
        let columns = if let Some(table) =
            self.catalog
                .table_resolved_with_control(self.resolution, name, control)?
        {
            table.columns.as_slice()
        } else if let Some(table) =
            self.catalog
                .foreign_table_resolved_with_control(self.resolution, name, control)?
        {
            table.columns.as_slice()
        } else {
            return Ok(None);
        };
        if columns.is_empty() {
            return self
                .fallback
                .try_table_column_names_with_control(name, control)
                .map(Some);
        }
        let mut output = uqa_core::memory::ProductionVec::new(*control);
        for column in columns {
            output.push_produced(control.copy_text(&column.name)?)?;
        }
        Ok(Some(output.finish()?))
    }

    fn with_view_definition_with_control(
        &self,
        name: &str,
        control: &uqa_core::memory::ProductionControl<'_>,
        read: &mut dyn FnMut(
            Option<uqa_sql::catalog::view::ViewRewriteDefinitionRef<'_>>,
        ) -> Result<(), SQLError>,
    ) -> Result<(), SQLError> {
        read(
            self.catalog
                .view_resolved_with_control(self.resolution, name, control)?
                .map(|view| view.rewrite_definition_ref()),
        )
    }
    fn try_resolve_view_name_with_control(
        &self,
        name: &str,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<Option<uqa_core::memory::Produced<String>>, SQLError> {
        self.catalog
            .view_name_resolved_with_control(self.resolution, name, control)
    }
    fn visit_rules_with_control(
        &self,
        name: &str,
        event: RuleEvent,
        reverse: bool,
        control: &uqa_core::memory::ProductionControl<'_>,
        visit: &mut dyn FnMut(&StoredRule) -> Result<(), SQLError>,
    ) -> Result<(), SQLError> {
        let relation = self.relation_with_control(name, control)?;
        selection::visit_active_rules_with_control(
            &self.catalog.snapshot().definitions.rules,
            &relation,
            event,
            self.replica,
            reverse,
            control,
            visit,
        )
    }
    fn has_trigger_definition_with_control(
        &self,
        name: &str,
        timing: TriggerTiming,
        event: TriggerEvent,
        row: bool,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<bool, SQLError> {
        selection::has_trigger_definition_with_control(
            &self.catalog.snapshot().definitions.triggers,
            &*self.relation_with_control(name, control)?,
            timing,
            event,
            row,
            control,
        )
    }
    fn rule_new_row_columns_with_control(
        &self,
        rule: &StoredRule,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<Option<uqa_core::memory::Produced<Vec<String>>>, SQLError> {
        self.fallback
            .rule_new_row_columns_with_control(rule, control)
    }
    fn target_view_kind_with_control(
        &self,
        name: &str,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<Option<StoredViewKind>, SQLError> {
        Ok(self
            .catalog
            .view_resolved_with_control(self.resolution, name, control)?
            .map(|view| view.kind))
    }
    fn binding_scope_with_control(
        &self,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<uqa_sql::binding::production::ControlledBindingSnapshot, SQLError> {
        let scope = self.restored_view_scope_with_control(control)?;
        Ok(uqa_sql::binding::production::ControlledBindingSnapshot {
            catalog: scope.catalog,
            resolution: scope.resolution,
            ctes: scope.ctes,
            deferred_ctes: scope.deferred_ctes,
            non_returning_ctes: scope.non_returning_ctes,
            scalar_subqueries: Arc::default(),
        })
    }
    fn restored_view_scope_with_control(
        &self,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<uqa_sql::binding::production::ControlledBindingContext<'static>, SQLError> {
        use uqa_sql::binding::production::{
            BindingMap, BindingNameSet, BindingResolution, ControlledBindingContext,
        };
        let catalog = control
            .finish(self.catalog.clone(), control.empty_reservation())?
            .into_shared(control)?;
        Ok(ControlledBindingContext {
            catalog,
            resolution: BindingResolution::from_produced(
                self.resolution.copy_with_control(control)?,
            ),
            ctes: BindingMap::new(),
            deferred_ctes: BindingMap::new(),
            non_returning_ctes: BindingNameSet::new(),
            scalar_subqueries: &[],
        })
    }
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
            scalar_subqueries: Arc::default(),
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
