//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Independent lexical maps retain private node/name leases while immutable statement inputs stay shared.

use super::{Arc, BindingMap, BindingNameSet, CteRowsMap, CteScope, ProductionControl, SQLError};

impl<S: Clone> CteScope<S> {
    /// Construct a complete independent lexical destination before exposing it. Plan, catalog, command and immutable row-lock metadata retain their existing owners; producer references remain weak, as in the original reader scopes.
    pub fn copy_with_control(&self, control: &ProductionControl<'_>) -> Result<Self, SQLError> {
        let mut output = self.copy_context_with_control(control)?;
        output.rows = self.rows.copy_with_control(control)?;
        output.deferred_ctes = self
            .deferred_ctes
            .copy_with_control(control, |plan| Ok(plan.clone()))?;
        output.definition_scopes = self
            .definition_scopes
            .copy_with_control(control, |definition| Ok(Arc::clone(definition)))?;
        output.folded_ctes = self.folded_ctes.copy_with_control(control)?;
        output.non_returning_ctes = self.non_returning_ctes.copy_with_control(control)?;
        output.visible_cte_names = self.visible_cte_names.copy_with_control(control)?;
        output.recursive_control_widths = self
            .recursive_control_widths
            .copy_with_control(control, |width| Ok(*width))?;
        Ok(output)
    }

    /// Copy only the invoking context so a definition can install its selected lexical inputs directly instead of copying and then discarding the current bindings.
    pub(in crate::query::scope) fn copy_context_with_control(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        control.check()?;
        Ok(Self {
            producers: self.producers.clone(),
            rows: CteRowsMap::new(),
            deferred_ctes: BindingMap::new(),
            definition_scopes: BindingMap::new(),
            folded_ctes: BindingNameSet::new(),
            non_returning_ctes: BindingNameSet::new(),
            scalar_subqueries: self.scalar_subqueries.clone(),
            source_outer_rewrites: self.source_outer_rewrites.clone(),
            lock_identities: self.lock_identities,
            row_lock: self.row_lock.clone(),
            visible_cte_names: BindingNameSet::new(),
            recursive_control_widths: BindingMap::new(),
            scalar_subquery_arena: self.scalar_subquery_arena,
            read_command_overlay: self.read_command_overlay,
            stream_command_progress: self.stream_command_progress,
            scan_backwards: self.scan_backwards,
            next_scalar_subquery_arena: Arc::clone(&self.next_scalar_subquery_arena),
            scalar_subquery_cache: Arc::clone(&self.scalar_subquery_cache),
            catalog: self.catalog.clone(),
            catalog_revision: self.catalog_revision,
            catalog_resolution: self.catalog_resolution.clone(),
            privilege_subject: self.privilege_subject.clone(),
            command_cte_snapshot: self.command_cte_snapshot.clone(),
            statement_commands: self.statement_commands.clone(),
        })
    }
}
