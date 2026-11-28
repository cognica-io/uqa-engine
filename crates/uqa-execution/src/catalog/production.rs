//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Immutable catalog capture admits the shared payloads before constructing their owners.

use super::{
    cache::CatalogDerivation,
    composite_type::relations::RelationDescriptorCache,
    projection::{DependencyCatalogCache, EventDefinitions, RoutineDefinitions},
    CatalogReadGeneration, CatalogReadSnapshot, CatalogReadView,
};
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
            let memory =
                control.reserve(size_of::<Produced<super::range_type::RangeDescriptors>>())?;
            Some(Box::new(control.finish(
                super::range_type::RangeDescriptors::default(),
                memory,
            )?))
        };
        let (snapshot, memory) = snapshot.into_parts();
        let generation = CatalogReadGeneration {
            snapshot,
            relation_descriptors: RelationDescriptorCache::default(),
            range_descriptors,
            dependencies: DependencyCatalogCache::default(),
            index_relations: CatalogDerivation::default(),
            constraint_definitions: CatalogDerivation::default(),
            event_definitions: EventDefinitions::default(),
            routine_definitions: RoutineDefinitions::default(),
            alias_output: CatalogDerivation::default(),
        };
        Ok(Self::from_generation(
            control.finish(generation, memory)?.into_shared(control)?,
        ))
    }

    pub(super) fn from_generation(snapshot: Arc<Produced<CatalogReadGeneration>>) -> Self {
        Self {
            snapshot,
            graph_reads: None,
            prepared_catalog: None,
            physical_catalog: None,
            sequence_positions: None,
            latest_sequence_values: None,
        }
    }

    pub fn downgrade_generation(&self) -> super::WeakCatalogReadGeneration {
        super::WeakCatalogReadGeneration(Arc::downgrade(&self.snapshot))
    }
}

mod tables;

mod capture;
pub use capture::CatalogReadCapture;
