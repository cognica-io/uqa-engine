//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native scope capture admits its new immutable namespace, shared execution state and lexical row names before construction.

use super::{BindingMap, BindingNameSet, CteScope, LockIdentityOptions};
mod copying;
use crate::{catalog::CatalogReadView, query::cte::rows::CteRows};
use std::{
    collections::BTreeMap,
    sync::{atomic::AtomicU64, Arc},
};
use uqa_core::memory::{Produced, ProductionControl};
use uqa_sql::{catalog::resolution::RelationNameResolution, SQLError};

mod ctes;
pub(super) mod resolution;
mod rows;
pub(super) mod subject;
pub use rows::{CteRowsIter, CteRowsMap};

pub(in crate::query::scope) fn shared<T>(
    value: T,
    control: &ProductionControl<'_>,
) -> Result<Arc<Produced<T>>, SQLError> {
    Ok(control
        .finish(value, control.empty_reservation())?
        .into_shared(control)?)
}

impl<S: Clone> CteScope<S> {
    pub(super) fn empty_with_control(control: &ProductionControl<'_>) -> Result<Self, SQLError> {
        control.check()?;
        Ok(Self {
            producers: super::producers::CteProducers::with_control(control)?,
            rows: CteRowsMap::new(),
            deferred_ctes: BindingMap::new(),
            definition_scopes: BindingMap::new(),
            folded_ctes: BindingNameSet::new(),
            non_returning_ctes: BindingNameSet::new(),
            scalar_subqueries: uqa_sql::plan::RetainedQueryArena::default(),
            source_outer_rewrites: None,
            lock_identities: LockIdentityOptions::default(),
            row_lock: None,
            visible_cte_names: BindingNameSet::new(),
            recursive_control_widths: BindingMap::new(),
            scalar_subquery_arena: 0,
            read_command_overlay: true,
            stream_command_progress: false,
            scan_backwards: false,
            next_scalar_subquery_arena: shared(AtomicU64::new(1), control)?,
            scalar_subquery_cache: shared(parking_lot::Mutex::new(BTreeMap::new()), control)?,
            catalog: None,
            catalog_revision: None,
            catalog_resolution: None,
            privilege_subject: None,
            command_cte_snapshot: None,
            statement_commands: None,
        })
    }

    /// Capture the current routine's immutable statement inputs. The catalog already owns its generation; new namespace and execution-state payloads keep their own shared leases.
    pub fn with_catalog_with_control(
        catalog: CatalogReadView,
        resolution: Produced<RelationNameResolution>,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        control.check()?;
        let resolution = resolution::ScopeResolution::capture(resolution, control)?;
        let mut scope = Self::empty_with_control(control)?;
        scope.catalog = Some(catalog);
        scope.catalog_resolution = Some(resolution);
        Ok(scope)
    }

    /// Replace relation authority only after its complete private reference has been admitted. SQL-visible `current_user` and namespace stay in their original owner.
    pub fn set_privilege_subject_with_control(
        &mut self,
        subject: Option<&uqa_sql::catalog::roles::RoleReference>,
        control: &ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        control.check()?;
        let subject = subject
            .map(|subject| subject::ScopeSubject::capture(subject, control))
            .transpose()?;
        self.privilege_subject = subject;
        Ok(())
    }

    pub fn relation_name_resolution_with_control(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<RelationNameResolution>, SQLError> {
        control.check()?;
        self.name_resolution()?.copy_with_control(control)
    }

    /// Retain a preexisting transition materialization, admitting only its new lexical name and map node. Replacing the same name reuses its existing identity allocation.
    pub fn insert_shared_with_control(
        &mut self,
        name: &str,
        rows: crate::SharedSpill,
        control: &ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        self.insert_materialized_with_control(name, rows, true, control)
    }

    /// Publish materialized rows and their command visibility together after all new name storage is admitted.
    pub fn insert_materialized_with_control(
        &mut self,
        name: &str,
        rows: crate::SharedSpill,
        returns_rows: bool,
        control: &ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        let mut non_returning = BindingNameSet::new();
        if !returns_rows && !self.non_returning_ctes.contains(name) {
            non_returning.insert_with_control(name, control)?;
        }
        self.rows
            .insert_with_control(name, CteRows::Complete(rows), control)?;
        self.deferred_ctes.remove(name);
        self.definition_scopes.remove(name);
        self.folded_ctes.remove(name);
        if returns_rows {
            self.non_returning_ctes.remove(name);
        }
        self.non_returning_ctes.append(non_returning);
        Ok(())
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod lexical_tests;
