//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Named analyzer publication keeps executable revisions and their encoded projection together.

use crate::{
    analyzer_image::AnalyzerImageSnapshot, read_control::StorageReadControl, StorageBackendResult,
};
use parking_lot::Mutex;
use std::sync::Arc;
use uqa_analysis::{AnalyzerImageSource, CompiledAnalyzer};
use uqa_core::memory::{
    Budgeted, MemoryBudget, MemoryError, MemoryReservation, OwnedMap, Produced, ProductionControl,
    RetainedMap, RetainedMapSnapshot,
};

/// Native paired roots. Only this owner can change membership; the encoded projection has no references to executable revisions or decoded dictionary models.
#[derive(Clone)]
pub struct AnalyzerCatalog {
    revisions: RetainedMap<Produced<String>, Arc<CompiledAnalyzer>>,
    images: RetainedMap<Produced<String>, AnalyzerImageSource>,
}

impl Default for AnalyzerCatalog {
    fn default() -> Self {
        Self::new(&MemoryBudget::new(usize::MAX))
    }
}

impl AnalyzerCatalog {
    pub fn new(memory: &MemoryBudget) -> Self {
        Self {
            revisions: RetainedMap::new(memory),
            images: RetainedMap::new(memory),
        }
    }

    pub fn get(&self, name: &str) -> Option<&Arc<CompiledAnalyzer>> {
        self.revisions.get(name)
    }

    pub fn contains_key(&self, name: &str) -> bool {
        self.get(name).is_some()
    }

    pub fn keys(&self) -> impl Iterator<Item = &String> {
        self.revisions.iter().map(|(name, _)| &**name)
    }

    pub fn len(&self) -> usize {
        self.revisions.len()
    }

    pub fn is_empty(&self) -> bool {
        self.revisions.is_empty()
    }

    /// Complete both immutable successors before installing either root. Failed construction leaves the published revisions and their encoded images unchanged.
    pub fn insert(
        &mut self,
        name: &str,
        compiled: Arc<CompiledAnalyzer>,
    ) -> StorageBackendResult<()> {
        self.insert_with_cancellation(name, compiled, &uqa_core::CancellationToken::new())
    }

    pub fn insert_with_cancellation(
        &mut self,
        name: &str,
        compiled: Arc<CompiledAnalyzer>,
        cancellation: &uqa_core::CancellationToken,
    ) -> StorageBackendResult<()> {
        let production = ProductionControl::new(self.images.budget(), cancellation, cancellation);
        let revision_name = production.copy_text(name)?;
        let image_name = production.copy_text(name)?;
        self.insert_names(revision_name, image_name, compiled, cancellation)
    }

    fn insert_names(
        &mut self,
        revision_name: Produced<String>,
        image_name: Produced<String>,
        compiled: Arc<CompiledAnalyzer>,
        cancellation: &uqa_core::CancellationToken,
    ) -> StorageBackendResult<()> {
        let production = ProductionControl::new(self.images.budget(), cancellation, cancellation);
        let image = compiled.image_source_with_control(&production)?;
        let payload_bytes = image_name
            .capacity()
            .checked_add(image.retained_bytes())
            .ok_or(MemoryError::SizeOverflow)?;
        let mut revisions = self.revisions.clone();
        let mut images = self.images.clone();
        let name_bytes = revision_name.capacity();
        revisions.insert(revision_name, compiled, name_bytes)?;
        images.insert(image_name, image, payload_bytes)?;
        cancellation.check()?;
        *self = Self { revisions, images };
        Ok(())
    }

    /// Adopt the decoded permanent name while preserving the compiled revision and its resource owners. Both native roots publish together; a failed input consumes this unpublished candidate.
    pub fn insert_image(
        mut self,
        name: Budgeted<String>,
        compiled: Arc<CompiledAnalyzer>,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        control.check()?;
        if !name.budget().shares_allowance(control.memory())
            || !self.images.budget().shares_allowance(control.memory())
            || !self.revisions.budget().shares_allowance(control.memory())
            || name.reserved_bytes() < name.capacity()
            || self.contains_key(name.as_str())
        {
            return Err(crate::StorageBackendError::Other(
                "invalid analyzer image admission or identity".into(),
            ));
        }
        let image_name = ProductionControl::new(
            control.memory(),
            control.cancellation(),
            control.cancellation(),
        )
        .copy_text(&name)?;
        self.insert_names(name.into(), image_name, compiled, control.cancellation())?;
        Ok(self)
    }

    pub fn into_mutation_budget(self, memory: &MemoryBudget) -> Self {
        Self {
            revisions: self.revisions.into_mutation_budget(memory),
            images: self.images.into_mutation_budget(memory),
        }
    }

    pub fn remove(&mut self, name: &str) -> StorageBackendResult<bool> {
        let mut candidate = self.clone();
        let removed = candidate.revisions.remove(name)?;
        let image_removed = candidate.images.remove(name)?;
        debug_assert_eq!(removed, image_removed);
        *self = candidate;
        Ok(removed)
    }

    pub fn clear(&mut self) {
        self.revisions.clear();
        self.images.clear();
    }

    pub fn extend(
        &mut self,
        values: impl IntoIterator<Item = (String, Arc<CompiledAnalyzer>)>,
    ) -> StorageBackendResult<()> {
        let mut candidate = self.clone();
        for (name, compiled) in values {
            candidate.insert(&name, compiled)?;
        }
        *self = candidate;
        Ok(())
    }

    /// Capture independently admits only the encoded root, using its cached complete footprint without walking names or resource bodies.
    pub fn capture(
        &self,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Budgeted<CapturedAnalyzers>> {
        control.check()?;
        let (values, remaining) = self.images.capture(control.memory())?.into_parts();
        control.check()?;
        Ok(Budgeted::new(
            CapturedAnalyzers {
                values,
                state: Mutex::new(CapturedState {
                    images: OwnedMap::new(),
                    remaining,
                }),
                control: control.clone(),
            },
            control.memory().empty_reservation(),
        ))
    }
}

impl std::ops::Index<&str> for AnalyzerCatalog {
    type Output = Arc<CompiledAnalyzer>;

    fn index(&self, name: &str) -> &Self::Output {
        self.get(name).expect("analyzer exists in this catalog")
    }
}

struct CapturedState {
    images: OwnedMap<String, Budgeted<AnalyzerImageSnapshot>>,
    remaining: MemoryReservation,
}

/// Export-only root. Escaping image children retain exactly their original encoded owner and allowance, independently of other names or later catalog publication.
pub struct CapturedAnalyzers {
    values: RetainedMapSnapshot<Produced<String>, AnalyzerImageSource>,
    state: Mutex<CapturedState>,
    control: StorageReadControl,
}

impl CapturedAnalyzers {
    /// Seek the next retained identity in logarithmic time, without allocating or visiting earlier entries.
    pub fn next_name(&self, after: Option<&str>) -> StorageBackendResult<Option<&str>> {
        self.control.check()?;
        Ok(self
            .values
            .first_from(after.map_or(std::ops::Bound::Unbounded, std::ops::Bound::Excluded))
            .map(|(name, _)| name.as_str()))
    }

    pub fn len(&self) -> usize {
        self.values.len()
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.values.iter().map(|(name, _)| name.as_str())
    }

    pub fn image(&self, name: &str) -> StorageBackendResult<Option<AnalyzerImageSnapshot>> {
        self.control.check()?;
        let Some(source) = self.values.get(name) else {
            return Ok(None);
        };
        let mut state = self.state.lock();
        self.control.check()?;
        if let Some(image) = state.images.get(name) {
            return Ok(Some((**image).clone()));
        }
        let production = ProductionControl::new(
            self.control.memory(),
            self.control.cancellation(),
            self.control.cancellation(),
        );
        let (key, key_memory) = production.copy_text(name)?.into_parts();
        let key_memory = key_memory.expect("analyzer image cache retains its capture allowance");
        let mut memory = self
            .control
            .memory()
            .reserve(AnalyzerImageSnapshot::wrapper_bytes())?;
        let CapturedState { images, remaining } = &mut *state;
        images.insert_budgeted_with(
            key,
            self.control.memory(),
            || -> StorageBackendResult<_> {
                self.control.check()?;
                memory.absorb(remaining.split(source.retained_bytes()));
                Ok(Budgeted::new(
                    AnalyzerImageSnapshot::from_admitted(source.clone(), &self.control, memory),
                    key_memory,
                ))
            },
        )?;
        Ok(Some(
            (**images.get(name).expect("selected analyzer was cached")).clone(),
        ))
    }
}

#[cfg(test)]
mod tests;
