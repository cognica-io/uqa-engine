//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stored statement and rewrite-rule column dependency lifecycle entry points.

use super::{
    BTreeSet, ColumnBindingContext, ColumnBindingMode, Engine, Expr, RelationIdentity,
    RuleColumnBinder, RuleColumnDependency, SQLError, Statement,
};

impl Engine {
    pub(crate) fn bind_stored_statement_source_columns(
        &self,
        statement: &mut Statement,
    ) -> Result<bool, SQLError> {
        let catalog = self.catalog_read_view();
        let mut resolution = self.session_execution_view().relation_name_resolution();
        resolution.set_lookup_mode(crate::engine_capabilities::RelationLookupMode::Bound);
        let mut changed = false;
        crate::engine_events::visit_stored_statement_sources(statement, &mut |source| {
            let uqa_sql::ast::FromClause::Table {
                name,
                bound_columns,
                ..
            } = source
            else {
                return Ok(());
            };
            if bound_columns.is_some() {
                return Ok(());
            }
            *bound_columns = if let Some(table) = catalog.table_resolved(&resolution, name)? {
                Some(
                    table
                        .columns
                        .iter()
                        .map(|column| column.name.clone())
                        .collect(),
                )
            } else {
                catalog
                    .foreign_table_resolved(&resolution, name)?
                    .map(|table| {
                        table
                            .columns
                            .iter()
                            .map(|column| column.name.clone())
                            .collect()
                    })
            };
            changed |= bound_columns.is_some();
            Ok(())
        })?;
        Ok(changed)
    }

    pub(crate) fn remove_stored_statement_source_column_aliases(
        &self,
        statement: &mut Statement,
        dependencies: &BTreeSet<RuleColumnDependency>,
    ) -> Result<bool, SQLError> {
        let mut planned = statement.clone();
        let mut binder = RuleColumnBinder::new(self, ColumnBindingMode::Drop { dependencies });
        binder.bind_statement(&mut planned, &[], &ColumnBindingContext::default())?;
        let changed = binder.alias_shape_changed();
        if changed {
            crate::engine_events::copy_stored_source_column_shapes(&mut planned, statement)?;
        }
        Ok(changed)
    }

    pub(crate) fn stored_statement_column_dependencies(
        &self,
        statement: &Statement,
    ) -> Result<BTreeSet<RuleColumnDependency>, SQLError> {
        self.bind_rule_action_column_dependencies(&mut statement.clone())
    }

    pub(in crate::engine_events) fn bind_rule_condition_column_dependencies(
        &self,
        condition: &mut Expr,
    ) -> Result<BTreeSet<RuleColumnDependency>, SQLError> {
        let mut binder = RuleColumnBinder::new(self, ColumnBindingMode::Bind);
        binder.bind_expr(condition, &[], &ColumnBindingContext::default())?;
        Ok(binder.finish())
    }

    pub(in crate::engine_events) fn bind_rule_action_column_dependencies(
        &self,
        action: &mut Statement,
    ) -> Result<BTreeSet<RuleColumnDependency>, SQLError> {
        let mut binder = RuleColumnBinder::new(self, ColumnBindingMode::Bind);
        binder.bind_statement(action, &[], &ColumnBindingContext::default())?;
        Ok(binder.finish())
    }

    pub(in crate::engine_events) fn rewrite_rule_column_references(
        &self,
        definition: &mut uqa_sql::ast::CreateRule,
        relation: &RelationIdentity,
        from: &str,
        to: &str,
    ) -> Result<(), SQLError> {
        let mode = ColumnBindingMode::Rename { relation, from, to };
        if let Some(condition) = &mut definition.condition {
            let mut binder = RuleColumnBinder::new(self, mode);
            binder.bind_expr(condition, &[], &ColumnBindingContext::default())?;
        }
        for action in &mut definition.actions {
            let mut binder = RuleColumnBinder::new(self, mode);
            binder.bind_statement(action, &[], &ColumnBindingContext::default())?;
        }
        Ok(())
    }

    pub(in crate::engine_events) fn remove_rule_source_column_aliases(
        &self,
        definition: &mut uqa_sql::ast::CreateRule,
        dependency: &RuleColumnDependency,
    ) -> Result<bool, SQLError> {
        let dependencies = BTreeSet::from([dependency.clone()]);
        let mode = ColumnBindingMode::Drop {
            dependencies: &dependencies,
        };
        let mut changed = false;
        if let Some(condition) = &mut definition.condition {
            let mut binder = RuleColumnBinder::new(self, mode);
            binder.bind_expr(condition, &[], &ColumnBindingContext::default())?;
            changed |= binder.alias_shape_changed();
        }
        for action in &mut definition.actions {
            let mut binder = RuleColumnBinder::new(self, mode);
            binder.bind_statement(action, &[], &ColumnBindingContext::default())?;
            changed |= binder.alias_shape_changed();
        }
        Ok(changed)
    }
}
