//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Immutable catalog capture admits the shared payloads before constructing their owners.

use super::{CatalogReadSnapshot, CatalogReadView};
use std::sync::Arc;
use uqa_core::memory::{Produced, ProductionControl};
use uqa_sql::SQLError;

impl CatalogReadView {
    pub fn new_with_control(
        snapshot: Produced<CatalogReadSnapshot>,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        control.check()?;
        let range_descriptors = if snapshot.definitions.ranges.is_empty() {
            None
        } else {
            Some(default_shared(control)?)
        };
        Ok(Self {
            snapshot: snapshot.into_shared(control)?,
            relation_descriptors: default_shared(control)?,
            range_descriptors,
            dependencies: default_shared(control)?,
            index_relations: default_shared(control)?,
            constraint_definitions: default_shared(control)?,
            event_definitions: default_shared(control)?,
            routine_definitions: default_shared(control)?,
            alias_output: default_shared(control)?,
            graph_reads: None,
            prepared_catalog: None,
            physical_catalog: None,
            sequence_positions: None,
            latest_sequence_values: None,
        })
    }
}

// These caches' default constructors contain only empty containers and synchronization fields.
fn default_shared<T: Default>(
    control: &ProductionControl<'_>,
) -> Result<Arc<Produced<T>>, SQLError> {
    let memory = control.reserve(size_of::<Produced<T>>())?;
    Ok(Arc::new(control.finish(T::default(), memory)?))
}
