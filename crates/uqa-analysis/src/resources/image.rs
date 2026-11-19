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

struct ImageResources<'a> {
    inputs: Vec<AnalyzerResourceInput>,
    #[cfg(any(feature = "nori", feature = "kuromoji"))]
    owner: &'a AnalyzerResources,
    #[cfg(not(any(feature = "nori", feature = "kuromoji")))]
    owner: std::marker::PhantomData<&'a AnalyzerResources>,
}

impl ImageResources<'_> {
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
        crate::work::inline(self.restore_image_with(descriptor, inputs, &mut ())).map(Arc::new)
    }

    /// Reconstruct the same private revision through the caller's original input work channel. Encoded artifacts retain their original admitted allocations; preparation performs no source reads, host resolution or cache publication.
    pub async fn restore_image_with_input(
        &self,
        descriptor: Arc<AnalyzerDescriptor>,
        inputs: Vec<AnalyzerResourceInput>,
        input: &mut uqa_core::image_io::AsyncImageInput<'_>,
    ) -> AnalysisResult<Arc<CompiledAnalyzer>> {
        self.restore_image_with(descriptor, inputs, input)
            .await
            .map(Arc::new)
    }

    /// Reconstruct a private, exclusively owned revision before native catalog publication. Its descriptor and resources follow the same exact validation as `restore_image_with_input`; no compiled or resource cache exposes this value during preparation.
    pub async fn restore_owned_image_with_input(
        &self,
        descriptor: Arc<AnalyzerDescriptor>,
        inputs: Vec<AnalyzerResourceInput>,
        input: &mut uqa_core::image_io::AsyncImageInput<'_>,
    ) -> AnalysisResult<CompiledAnalyzer> {
        self.restore_image_with(descriptor, inputs, input).await
    }

    async fn restore_image_with(
        &self,
        descriptor: Arc<AnalyzerDescriptor>,
        inputs: Vec<AnalyzerResourceInput>,
        work: &mut impl crate::work::ResourceWork,
    ) -> AnalysisResult<CompiledAnalyzer> {
        crate::descriptor::limits::check_limit(
            "image dictionary artifacts",
            inputs.len(),
            self.limits().max_stages.saturating_add(2),
        )?;
        for (index, pair) in inputs.windows(2).enumerate() {
            crate::work::Work::<AnalysisError>::records(work, index).await?;
            if (pair[0].kind, pair[0].sha256) >= (pair[1].kind, pair[1].sha256) {
                return Err(AnalysisError::Descriptor(
                    "unordered or duplicate image resources",
                ));
            }
        }
        for (index, input) in inputs.iter().enumerate() {
            crate::work::Work::<AnalysisError>::records(work, index).await?;
            crate::descriptor::limits::check_limit(
                "image dictionary bytes",
                input.bytes.len(),
                self.image_resource_byte_limit(input.kind)?,
            )?;
        }
        let resources = ImageResources {
            inputs,
            #[cfg(any(feature = "nori", feature = "kuromoji"))]
            owner: self,
            #[cfg(not(any(feature = "nori", feature = "kuromoji")))]
            owner: std::marker::PhantomData,
        };
        let config = descriptor
            .validated_configuration_with(self.limits(), work)
            .await?;
        #[cfg(any(feature = "nori", feature = "kuromoji"))]
        let mut config = config;
        #[cfg(feature = "nori")]
        let nori = {
            crate::nori::pipeline::check_resolved_with(&config, work).await?;
            crate::nori::pipeline::ResolvedNoriPipeline::resolve_with(&mut config, &resources, work)
                .await?
        };
        #[cfg(feature = "kuromoji")]
        let kuromoji = {
            crate::kuromoji::pipeline::check_resolved_with(&config, work).await?;
            crate::kuromoji::pipeline::ResolvedKuromojiPipeline::resolve_with(
                &mut config,
                &resources,
                self.limits(),
                work,
            )
            .await?
        };
        let compiled = CompiledAnalyzer::prepare_configuration_with(
            descriptor,
            config,
            #[cfg(feature = "nori")]
            nori,
            #[cfg(feature = "kuromoji")]
            kuromoji,
            work,
        )
        .await?;
        let mut actual = compiled.dictionary_resources();
        for (index, expected) in resources.inputs.iter().enumerate() {
            crate::work::Work::<AnalysisError>::records(work, index).await?;
            if actual
                .next()
                .map(|resource| (resource.kind(), resource.sha256()))
                != Some((expected.kind, expected.sha256))
            {
                return Err(AnalysisError::Descriptor(
                    "image resources differ from the descriptor closure",
                ));
            }
        }
        if actual.next().is_some() {
            return Err(AnalysisError::Descriptor(
                "image resources differ from the descriptor closure",
            ));
        }
        drop(actual);
        Ok(compiled)
    }
}

#[cfg(test)]
mod tests;

#[cfg(any(feature = "nori", feature = "kuromoji"))]
mod preparation;
