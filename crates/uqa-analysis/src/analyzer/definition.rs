//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Immutable unresolved configuration publication keeps its encoded image independently of executable resources.

use super::Analyzer;
use crate::AnalysisError;
use std::sync::Arc;
use uqa_core::{
    memory::{MemoryError, Produced, ProductionControl},
    ValueRetentionError,
};

/// An original configuration and its publication-time image. Construction neither validates stages nor resolves files or dictionaries. Encoding failures remain attached to this generation and affect capture only.
#[derive(Debug, Clone)]
pub struct AnalyzerDefinition {
    configuration: Arc<Analyzer>,
    image: Result<AnalyzerConfigurationImage, Arc<AnalysisError>>,
}

/// Image construction errors belong to their original generation; independent retention preserves Core's exact admission and cancellation errors.
#[derive(Debug, thiserror::Error)]
pub enum AnalyzerConfigurationError {
    #[error(transparent)]
    Encoding(Arc<AnalysisError>),
    #[error(transparent)]
    Admission(#[from] ValueRetentionError),
}

/// Only the unresolved configuration bytes. Retaining this image does not retain the original configuration, compiled pipelines or resolved resources.
#[derive(Debug, Clone)]
pub struct AnalyzerConfigurationImage {
    bytes: Arc<Vec<u8>>,
    retained_bytes: usize,
}

impl AnalyzerDefinition {
    pub fn new(configuration: Analyzer) -> Self {
        let image = serde_json::to_vec(&configuration)
            .map_err(AnalysisError::from)
            .and_then(|bytes| {
                let retained_bytes = bytes
                    .capacity()
                    .checked_add(size_of::<Vec<u8>>())
                    .ok_or(MemoryError::SizeOverflow)?;
                Ok(AnalyzerConfigurationImage {
                    bytes: Arc::new(bytes),
                    retained_bytes,
                })
            })
            .map_err(Arc::new);
        Self {
            configuration: Arc::new(configuration),
            image,
        }
    }

    pub fn configuration(&self) -> &Analyzer {
        &self.configuration
    }

    /// Independently admit and share the original encoded owner without serialization, resource resolution or population-dependent work.
    pub fn capture_image(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<AnalyzerConfigurationImage>, AnalyzerConfigurationError> {
        control.check()?;
        let image = self
            .image
            .as_ref()
            .map_err(|error| AnalyzerConfigurationError::Encoding(Arc::clone(error)))?;
        let memory = control.reserve(image.retained_bytes)?;
        Ok(control.finish(image.clone(), memory)?)
    }
}

impl serde::Serialize for AnalyzerDefinition {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.configuration().serialize(serializer)
    }
}

impl AnalyzerConfigurationImage {
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }
}

impl std::ops::Deref for AnalyzerConfigurationImage {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        self.as_bytes()
    }
}

#[cfg(test)]
mod tests;
