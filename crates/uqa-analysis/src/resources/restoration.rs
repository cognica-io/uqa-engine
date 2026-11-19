//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Persistent descriptors reuse the original resource owner and native preparation schedule.

use super::AnalyzerResources;
use crate::{AnalysisError, AnalysisResult, AnalyzerDescriptor, CompiledAnalyzer};
use std::sync::Arc;
use uqa_core::image_io::AsyncImageInput;

impl AnalyzerResources {
    /// Restore a persisted descriptor through its enclosing work channel. Descriptor validation precedes cache lookup; missing compiled revisions resolve only the descriptor's exact resource hashes through this owner's existing resolvers. Model allocations retain their existing Analysis limits and ownership.
    pub async fn restore_json_with_input(
        &self,
        json: &str,
        input: &mut AsyncImageInput<'_>,
    ) -> AnalysisResult<Arc<CompiledAnalyzer>> {
        let descriptor =
            AnalyzerDescriptor::from_json_with_input(json, self.limits(), input).await?;
        self.restore_with(descriptor, input).await
    }

    pub(super) async fn restore_with(
        &self,
        descriptor: Arc<AnalyzerDescriptor>,
        work: &mut impl crate::work::ResourceWork,
    ) -> AnalysisResult<Arc<CompiledAnalyzer>> {
        let config = descriptor
            .validated_configuration_with(self.limits(), work)
            .await?;
        if let Some(compiled) = self.0.cache.lock().get(&descriptor.fingerprint()) {
            return Ok(compiled);
        }
        #[cfg(any(feature = "nori", feature = "kuromoji"))]
        let mut config = config;
        #[cfg(feature = "nori")]
        let nori = {
            crate::nori::pipeline::check_resolved_with(&config, work).await?;
            crate::nori::pipeline::ResolvedNoriPipeline::resolve_with(
                &mut config,
                &self.0.nori,
                work,
            )
            .await?
        };
        #[cfg(feature = "kuromoji")]
        let kuromoji = {
            crate::kuromoji::pipeline::check_resolved_with(&config, work).await?;
            crate::kuromoji::pipeline::ResolvedKuromojiPipeline::resolve_with(
                &mut config,
                &self.0.kuromoji,
                self.limits(),
                work,
            )
            .await?
        };
        crate::work::Work::<AnalysisError>::checkpoint(work).await?;
        if let Some(compiled) = self.0.cache.lock().get(&descriptor.fingerprint()) {
            return Ok(compiled);
        }
        let fingerprint = descriptor.fingerprint();
        let weight = descriptor.canonical_json().len();
        let compiled = Arc::new(
            CompiledAnalyzer::prepare_configuration_with(
                descriptor,
                config,
                #[cfg(feature = "nori")]
                nori,
                #[cfg(feature = "kuromoji")]
                kuromoji,
                work,
            )
            .await?,
        );
        crate::work::Work::<AnalysisError>::checkpoint(work).await?;
        let mut cache = self.0.cache.lock();
        // Another retained reader may have completed the same immutable revision while this one yielded. Publish one shared cache identity without retaining a lock across preparation.
        if let Some(current) = cache.get(&fingerprint) {
            return Ok(current);
        }
        cache.insert(
            fingerprint,
            compiled.clone(),
            weight,
            self.limits().max_cached_analyzers,
            self.limits().max_cached_descriptor_bytes,
        );
        Ok(compiled)
    }
}

#[cfg(test)]
mod tests;
