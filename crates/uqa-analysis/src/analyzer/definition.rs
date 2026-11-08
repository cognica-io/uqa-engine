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
    json::JsonReadError,
    memory::{Budgeted, MemoryError, Produced, ProductionControl},
    CancellationToken, ValueRetentionError,
};

mod image;

/// An original configuration and its publication-time image. Construction neither validates stages nor resolves files or dictionaries. Encoding failures remain attached to this generation and affect capture only.
#[derive(Debug, Clone)]
pub struct AnalyzerDefinition {
    configuration: Arc<Produced<Analyzer>>,
    image: Result<AnalyzerConfigurationImage, Arc<AnalysisError>>,
}

/// Image construction errors belong to their original generation; independent retention preserves Core's exact admission and cancellation errors.
#[derive(Debug, thiserror::Error)]
pub enum AnalyzerConfigurationError {
    #[error(transparent)]
    Encoding(Arc<AnalysisError>),
    #[error(transparent)]
    Admission(#[from] ValueRetentionError),
    #[error(transparent)]
    Decoding(#[from] JsonReadError),
}

/// Only the unresolved configuration bytes. Retaining this image does not retain the original configuration, compiled pipelines or resolved resources.
#[derive(Debug, Clone)]
pub struct AnalyzerConfigurationImage {
    bytes: Arc<Produced<Vec<u8>>>,
    retained_bytes: usize,
}

impl AnalyzerDefinition {
    pub fn new(configuration: Analyzer) -> Self {
        let image = serde_json::to_vec(&configuration)
            .map_err(AnalysisError::from)
            .and_then(|bytes| {
                let retained_bytes = bytes
                    .capacity()
                    .checked_add(size_of::<Produced<Vec<u8>>>())
                    .ok_or(MemoryError::SizeOverflow)?;
                Ok(AnalyzerConfigurationImage {
                    bytes: ordinary(bytes),
                    retained_bytes,
                })
            })
            .map_err(Arc::new);
        Self {
            configuration: ordinary(configuration),
            image,
        }
    }

    pub fn configuration(&self) -> &Analyzer {
        &self.configuration
    }

    /// Reconstruct this owner's original configuration image without validating stages, opening paths, resolving dictionaries or compiling a pipeline. The encoded buffer moves into the result unchanged; decoded native payloads and shared headers use its existing allowance. Every cloned definition retains both owners, while captured images retain only their encoded owner.
    pub fn from_image(
        bytes: Budgeted<Vec<u8>>,
        cancellation: &CancellationToken,
    ) -> Result<Self, AnalyzerConfigurationError> {
        let (bytes, memory) = bytes.into_parts();
        let budget = memory.budget().clone();
        let bytes = Budgeted::new(bytes, memory);
        let control = ProductionControl::new(&budget, cancellation, cancellation);
        control.check()?;
        let configuration = image::decode(&bytes, &budget, cancellation)?;
        let configuration = Produced::from(configuration).into_shared(&control)?;
        let retained_bytes = bytes
            .capacity()
            .checked_add(size_of::<Produced<Vec<u8>>>())
            .ok_or(ValueRetentionError::Memory(MemoryError::SizeOverflow))?;
        let bytes = Produced::from(bytes).into_shared(&control)?;
        Ok(Self {
            configuration,
            image: Ok(AnalyzerConfigurationImage {
                bytes,
                retained_bytes,
            }),
        })
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

fn ordinary<T>(value: T) -> Arc<Produced<T>> {
    let control = ProductionControl::uncontrolled();
    control
        .finish(value, None)
        .expect("ordinary configuration production")
        .into_shared(&control)
        .expect("ordinary configuration publication")
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
