//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native scope capture admits its new immutable namespace, shared execution state and lexical row names before construction.

use super::{CteScope, LockIdentityOptions};
use crate::{catalog::CatalogReadView, query::cte::rows::CteRows};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{atomic::AtomicU64, Arc},
};
use uqa_core::memory::{Produced, ProductionControl};
use uqa_sql::{catalog::resolution::RelationNameResolution, SQLError};

mod ctes;
pub(super) mod resolution;
mod rows;
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
            deferred_ctes: BTreeMap::new(),
            definition_scopes: BTreeMap::new(),
            folded_ctes: BTreeSet::new(),
            non_returning_ctes: BTreeSet::new(),
            scalar_subqueries: uqa_sql::plan::RetainedQueryArena::default(),
            source_outer_rewrites: None,
            lock_identities: LockIdentityOptions::default(),
            row_lock: None,
            visible_cte_names: BTreeSet::new(),
            recursive_control_widths: BTreeMap::new(),
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

    pub fn relation_name_resolution_with_control(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<RelationNameResolution>, SQLError> {
        control.check()?;
        self.catalog_resolution
            .as_ref()
            .ok_or_else(|| {
                SQLError::Internal(
                    "query execution scope has no statement name-resolution snapshot".into(),
                )
            })?
            .copy_with_control(control)
    }

    /// Retain a preexisting transition materialization, admitting only its new lexical name and map node. Replacing the same name reuses its existing identity allocation.
    pub fn insert_shared_with_control(
        &mut self,
        name: &str,
        rows: crate::SharedSpill,
        control: &ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        self.rows
            .insert_with_control(name, CteRows::Complete(rows), control)?;
        self.deferred_ctes.remove(name);
        self.definition_scopes.remove(name);
        self.folded_ctes.remove(name);
        self.non_returning_ctes.remove(name);
        Ok(())
    }
}

#[cfg(test)]
mod tests;
