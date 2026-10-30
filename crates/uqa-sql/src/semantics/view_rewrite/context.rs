//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Catalog and authorization inputs for automatic view analysis and DML rewriting.

use crate::binding::production::{ControlledBindingContext, ControlledBindingSnapshot};
use crate::catalog::view::ViewRewriteDefinitionRef;
use crate::{
    ast::{ColumnDef, RuleEvent, TriggerEvent, TriggerTiming},
    binding::snapshot::BindingSnapshot,
    catalog::{
        analysis::CatalogReadView,
        events::StoredRule,
        resolution::RelationNameResolution,
        view::{StoredViewKind, ViewRewriteDefinition},
    },
    semantics::sets::SetFunctionCatalog,
    RowSchema, SQLError,
};
use std::collections::BTreeSet;
use uqa_core::memory::{Produced, ProductionControl, ProductionVec};

pub trait ViewRewriteCatalog {
    fn view_definition(&self, name: &str) -> Result<Option<ViewRewriteDefinition>, SQLError>;
    fn try_resolve_view_name(&self, name: &str) -> Result<Option<String>, String>;
    fn try_describe_table(&self, name: &str) -> Result<Option<Vec<ColumnDef>>, String>;
    fn try_table_columns(&self, name: &str) -> Result<Vec<String>, String>;
    fn rules_for(&self, name: &str, event: RuleEvent) -> Result<Vec<StoredRule>, SQLError>;
    fn capture_rules(
        &self,
        name: &str,
        event: RuleEvent,
    ) -> Result<crate::catalog::events::selection::CapturedRules, SQLError> {
        self.capture_rules_with_control(name, event, &ProductionControl::uncontrolled())
    }
    fn capture_rules_with_control(
        &self,
        name: &str,
        event: RuleEvent,
        control: &ProductionControl<'_>,
    ) -> Result<crate::catalog::events::selection::CapturedRules, SQLError> {
        control.check()?;
        crate::catalog::events::selection::CapturedRules::from_external(
            self.rules_for(name, event)?,
            control,
        )
    }
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

    /// External catalogs may return owned declarations; native catalogs lend fields from their retained immutable definition.
    fn with_view_definition_with_control(
        &self,
        name: &str,
        control: &ProductionControl<'_>,
        read: &mut dyn FnMut(Option<ViewRewriteDefinitionRef<'_>>) -> Result<(), SQLError>,
    ) -> Result<(), SQLError> {
        control.check()?;
        let definition = self.view_definition(name)?;
        read(definition.as_ref().map(ViewRewriteDefinitionRef::from))?;
        Ok(control.check()?)
    }

    fn try_resolve_view_name_with_control(
        &self,
        name: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Produced<String>>, SQLError> {
        control.check()?;
        self.try_resolve_view_name(name)
            .map_err(|error| SQLError::Internal(format!("resolve DML view `{name}`: {error}")))?
            .map(|name| control.copy_text(&name).map_err(Into::into))
            .transpose()
    }

    /// External table catalogs may return owned names. Native adapters publish each selected declaration directly.
    fn try_table_column_names_with_control(
        &self,
        name: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Vec<String>>, SQLError> {
        control.check()?;
        let mut output = ProductionVec::new(*control);
        for name in self.try_table_columns(name).map_err(|error| {
            SQLError::Internal(format!("describe view source `{name}`: {error}"))
        })? {
            output.push_produced(control.copy_text(&name)?)?;
        }
        Ok(output.finish()?)
    }

    fn try_relation_column_names_with_control(
        &self,
        name: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Produced<Vec<String>>>, SQLError> {
        control.check()?;
        let Some(definitions) = self.try_describe_table(name).map_err(|error| {
            SQLError::Internal(format!("describe view source `{name}`: {error}"))
        })?
        else {
            return Ok(None);
        };
        let mut output = ProductionVec::new(*control);
        if definitions.is_empty() {
            return self
                .try_table_column_names_with_control(name, control)
                .map(Some);
        }
        for definition in definitions {
            output.push_produced(control.copy_text(&definition.name)?)?;
        }
        Ok(Some(output.finish()?))
    }

    fn visit_rules_with_control(
        &self,
        name: &str,
        event: RuleEvent,
        reverse: bool,
        control: &ProductionControl<'_>,
        visit: &mut dyn FnMut(&StoredRule) -> Result<(), SQLError>,
    ) -> Result<(), SQLError> {
        control.check()?;
        let rules = self.rules_for(name, event)?;
        if reverse {
            for rule in rules.iter().rev() {
                control.check()?;
                visit(rule)?;
            }
        } else {
            for rule in &rules {
                control.check()?;
                visit(rule)?;
            }
        }
        Ok(())
    }

    fn rule_new_row_columns_with_control(
        &self,
        rule: &StoredRule,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Produced<Vec<String>>>, SQLError> {
        control.check()?;
        let Some(columns) = self.rule_new_row_columns(rule)? else {
            return Ok(None);
        };
        let mut output = ProductionVec::new(*control);
        for column in columns {
            output.push_produced(control.copy_text(&column)?)?;
        }
        Ok(Some(output.finish()?))
    }

    fn has_trigger_definition_with_control(
        &self,
        name: &str,
        timing: TriggerTiming,
        event: TriggerEvent,
        row: bool,
        control: &ProductionControl<'_>,
    ) -> Result<bool, SQLError> {
        control.check()?;
        let result = self.has_trigger_definition(name, timing, event, row)?;
        control.check()?;
        Ok(result)
    }

    fn target_view_kind_with_control(
        &self,
        name: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Option<StoredViewKind>, SQLError> {
        control.check()?;
        let result = self.target_view_kind(name)?;
        control.check()?;
        Ok(result)
    }

    fn binding_scope_with_control(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<ControlledBindingSnapshot, SQLError> {
        control.check()?;
        self.binding_scope()?.copy_with_control(control)
    }

    fn restored_view_scope_with_control(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<ControlledBindingContext<'static>, SQLError> {
        control.check()?;
        let (catalog, resolution) = self.restored_view_catalog();
        Ok(ControlledBindingContext {
            catalog,
            resolution: crate::binding::production::BindingResolution::from_produced(
                resolution.copy_with_control(control)?,
            ),
            ctes: crate::binding::production::BindingMap::new(),
            deferred_ctes: crate::binding::production::BindingMap::new(),
            non_returning_ctes: crate::binding::production::BindingNameSet::new(),
            scalar_subqueries: &[],
        })
    }
}

#[derive(Clone, Copy)]
pub struct ViewRewriteContext<'a> {
    pub catalog: &'a dyn ViewRewriteCatalog,
    pub routines: &'a dyn SetFunctionCatalog,
    pub authorization: &'a dyn super::super::view_privileges::ViewPrivilegeCatalog,
}

pub fn stored_view_schema(
    services: ViewRewriteContext<'_>,
    definition: &ViewRewriteDefinition,
) -> Result<RowSchema, SQLError> {
    let (catalog, resolution) = services.catalog.restored_view_catalog();
    definition.row_schema(services.routines, catalog, resolution)
}

pub fn stored_view_schema_with_control(
    services: ViewRewriteContext<'_>,
    definition: ViewRewriteDefinitionRef<'_>,
    control: &ProductionControl<'_>,
) -> Result<RowSchema, SQLError> {
    definition.row_schema_with_control(
        services.routines,
        services.catalog.restored_view_scope_with_control(control)?,
        control,
    )
}
