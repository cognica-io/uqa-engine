//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A restored lazy default retains the original admitted configuration without resolving it.

use super::{AnalyzerDefault, Configuration, DefaultRevision};
use crate::{read_control::StorageReadControl, StorageBackendResult};
use std::sync::{Arc, OnceLock};
use uqa_analysis::{AnalyzerDefinition, AnalyzerResources};

#[cfg(test)]
mod tests;

impl AnalyzerDefault {
    /// Share a decoded configuration and admit the lazy resolver header before allocation. Construction does not validate stages, open files, resolve resources or retain input cancellation. Ordinary first-use resolution keeps its existing runtime behavior.
    pub fn from_definition(
        configuration: AnalyzerDefinition,
        resources: AnalyzerResources,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        control.check()?;
        let memory = control.memory().reserve(size_of::<DefaultRevision>())?;
        let value = Self(Arc::new(DefaultRevision {
            configuration: Configuration::Image(configuration),
            resources,
            resolved: OnceLock::new(),
            _image_memory: Some(memory),
        }));
        control.check()?;
        Ok(value)
    }
}
