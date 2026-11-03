//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Image reconstruction resolves exact artifacts only from the supplied immutable closure.

use super::AnalyzerResources;
use crate::{
    AnalysisError, AnalysisResult, AnalyzerDescriptor, AnalyzerResourceKind, CompiledAnalyzer,
};
use std::sync::Arc;

/// Untrusted encoded input. Complete reconstruction verifies its requested hash, dictionary format and reachability before returning a compiled revision.
pub struct AnalyzerResourceInput {
    pub kind: AnalyzerResourceKind,
    pub sha256: [u8; 32],
    pub bytes: Arc<uqa_core::memory::Budgeted<Vec<u8>>>,
}

struct ImageResources {
    inputs: Vec<AnalyzerResourceInput>,
}

impl ImageResources {
    #[cfg(any(feature = "nori", feature = "kuromoji"))]
    fn find(&self, kind: AnalyzerResourceKind, sha256: [u8; 32]) -> Option<&AnalyzerResourceInput> {
        self.inputs
            .binary_search_by_key(&(kind, sha256), |input| (input.kind, input.sha256))
            .ok()
            .map(|index| &self.inputs[index])
    }
}

impl AnalyzerResources {
    /// Maximum encoded payload accepted for one resource by this destination. Missing language features fail before a streaming importer allocates its payload.
    pub fn image_resource_byte_limit(&self, kind: AnalyzerResourceKind) -> AnalysisResult<usize> {
        match kind {
            #[cfg(feature = "nori")]
            AnalyzerResourceKind::Nori => {
                Ok(self.nori_resources().limits().dictionary.max_encoded_bytes)
            }
            #[cfg(feature = "kuromoji")]
            AnalyzerResourceKind::Kuromoji => Ok(self
                .kuromoji_resources()
                .limits()
                .dictionary
                .max_encoded_bytes),
            #[cfg(not(feature = "nori"))]
            AnalyzerResourceKind::Nori => Err(AnalysisError::Descriptor(
                "Nori image resources are unavailable in this build",
            )),
            #[cfg(not(feature = "kuromoji"))]
            AnalyzerResourceKind::Kuromoji => Err(AnalysisError::Descriptor(
                "Kuromoji image resources are unavailable in this build",
            )),
        }
    }

    /// Restore one descriptor using only its supplied dictionary images and this owner's validation limits. Names, host resolvers and existing compiled caches cannot supply missing artifacts. Inputs must be strictly ordered by kind/hash and contain exactly the descriptor's dependencies.
    pub fn restore_image(
        &self,
        descriptor: Arc<AnalyzerDescriptor>,
        inputs: Vec<AnalyzerResourceInput>,
    ) -> AnalysisResult<Arc<CompiledAnalyzer>> {
        crate::descriptor::limits::check_limit(
            "image dictionary artifacts",
            inputs.len(),
            self.limits().max_stages.saturating_add(2),
        )?;
        if inputs
            .windows(2)
            .any(|pair| (pair[0].kind, pair[0].sha256) >= (pair[1].kind, pair[1].sha256))
        {
            return Err(AnalysisError::Descriptor(
                "unordered or duplicate image resources",
            ));
        }
        for input in &inputs {
            crate::descriptor::limits::check_limit(
                "image dictionary bytes",
                input.bytes.len(),
                self.image_resource_byte_limit(input.kind)?,
            )?;
        }
        let resources = Arc::new(ImageResources { inputs });
        let owner = AnalyzerResources::builder(self.limits());
        #[cfg(feature = "nori")]
        let owner = owner.nori_resources(crate::nori::NoriResources::with_resolver(
            resources.clone(),
            self.nori_resources().limits(),
        ));
        #[cfg(feature = "kuromoji")]
        let owner = owner.kuromoji_resources(crate::kuromoji::KuromojiResources::with_resolver(
            resources.clone(),
            self.kuromoji_resources().limits(),
        ));
        let compiled = owner.build().restore(descriptor)?;
        if !compiled
            .dictionary_resources()
            .map(|resource| (resource.kind(), resource.sha256()))
            .eq(resources
                .inputs
                .iter()
                .map(|input| (input.kind, input.sha256)))
        {
            return Err(AnalysisError::Descriptor(
                "image resources differ from the descriptor closure",
            ));
        }
        Ok(compiled)
    }
}

#[cfg(test)]
mod tests;

#[cfg(feature = "nori")]
impl crate::nori::DictionaryResolver for ImageResources {
    fn resolve(
        &self,
        request: &crate::nori::DictionaryRequest,
    ) -> crate::nori::DictionaryResult<Option<crate::nori::DictionaryArtifact>> {
        let crate::nori::DictionaryRequest::Sha256(hash) = request else {
            return Ok(None);
        };
        Ok(self
            .find(AnalyzerResourceKind::Nori, hash.into_bytes())
            .map(|input| crate::nori::DictionaryArtifact {
                sha256: *hash,
                bytes: crate::nori::DictionaryBytes::Retained(input.bytes.clone()),
            }))
    }
}

#[cfg(feature = "kuromoji")]
impl crate::kuromoji::DictionaryResolver for ImageResources {
    fn resolve(
        &self,
        request: &crate::kuromoji::DictionaryRequest,
    ) -> crate::kuromoji::DictionaryResult<Option<crate::kuromoji::DictionaryArtifact>> {
        let crate::kuromoji::DictionaryRequest::Sha256(hash) = request else {
            return Ok(None);
        };
        Ok(self
            .find(AnalyzerResourceKind::Kuromoji, hash.into_bytes())
            .map(|input| crate::kuromoji::DictionaryArtifact {
                sha256: *hash,
                bytes: crate::kuromoji::DictionaryBytes::Retained(input.bytes.clone()),
            }))
    }
}
