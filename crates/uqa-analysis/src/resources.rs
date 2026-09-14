//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bounded ownership of immutable compiled analyzer revisions.

use std::sync::{Arc, OnceLock};

use parking_lot::Mutex;

use crate::{
    cache::Cache, AnalysisResult, Analyzer, AnalyzerDescriptor, AnalyzerFingerprint,
    AnalyzerLimits, CompiledAnalyzer, TokenLengthPolicy,
};

struct Inner {
    limits: AnalyzerLimits,
    cache: Mutex<Cache<AnalyzerFingerprint, CompiledAnalyzer>>,
    #[cfg(feature = "nori")]
    nori: crate::nori::NoriResources,
    #[cfg(feature = "kuromoji")]
    kuromoji: crate::kuromoji::KuromojiResources,
}

/// Retained descriptor sizes exclude compiled heap allocations and caller-owned handles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AnalyzerCacheStats {
    pub analyzers: usize,
    pub descriptor_bytes: usize,
}

/// Cloneable compilation owner; resolved revisions remain valid after cache eviction.
///
/// ```
/// use uqa_analysis::{standard_analyzer, AnalyzerLimits, AnalyzerResources};
/// let resources = AnalyzerResources::new(AnalyzerLimits::default());
/// let compiled = resources.compile(&standard_analyzer("english"))?;
/// let saved = compiled.descriptor().canonical_json();
/// let restored = resources.restore_json(saved)?;
/// assert!(std::sync::Arc::ptr_eq(&compiled, &restored));
/// assert_eq!(restored.analyze("The cats and")?, ["cat"]);
/// # Ok::<(), uqa_analysis::AnalysisError>(())
/// ```
#[derive(Clone)]
pub struct AnalyzerResources(Arc<Inner>);

/// Install independently typed language resolvers before creating an immutable analyzer owner.
pub struct AnalyzerResourcesBuilder {
    limits: AnalyzerLimits,
    #[cfg(feature = "nori")]
    nori: Option<crate::nori::NoriResources>,
    #[cfg(feature = "kuromoji")]
    kuromoji: Option<crate::kuromoji::KuromojiResources>,
}

impl AnalyzerResourcesBuilder {
    #[cfg(feature = "nori")]
    pub fn nori_resources(mut self, resources: crate::nori::NoriResources) -> Self {
        self.nori = Some(resources);
        self
    }

    #[cfg(feature = "kuromoji")]
    pub fn kuromoji_resources(mut self, resources: crate::kuromoji::KuromojiResources) -> Self {
        self.kuromoji = Some(resources);
        self
    }

    pub fn build(self) -> AnalyzerResources {
        AnalyzerResources(Arc::new(Inner {
            limits: self.limits,
            cache: Mutex::new(Cache::default()),
            #[cfg(feature = "nori")]
            nori: self.nori.unwrap_or_default(),
            #[cfg(feature = "kuromoji")]
            kuromoji: self.kuromoji.unwrap_or_default(),
        }))
    }
}

impl Default for AnalyzerResources {
    fn default() -> Self {
        static RESOURCES: OnceLock<AnalyzerResources> = OnceLock::new();
        RESOURCES
            .get_or_init(|| Self::new(AnalyzerLimits::default()))
            .clone()
    }
}

impl AnalyzerResources {
    /// Create an independent owner with fixed descriptor and retention limits.
    pub fn new(limits: AnalyzerLimits) -> Self {
        Self::builder(limits).build()
    }

    pub fn builder(limits: AnalyzerLimits) -> AnalyzerResourcesBuilder {
        AnalyzerResourcesBuilder {
            limits,
            #[cfg(feature = "nori")]
            nori: None,
            #[cfg(feature = "kuromoji")]
            kuromoji: None,
        }
    }

    /// Use explicit immutable Korean resources without introducing a fallback resolver.
    #[cfg(feature = "nori")]
    pub fn with_nori_resources(limits: AnalyzerLimits, nori: crate::nori::NoriResources) -> Self {
        Self::builder(limits).nori_resources(nori).build()
    }

    #[cfg(feature = "nori")]
    pub fn nori_resources(&self) -> &crate::nori::NoriResources {
        &self.0.nori
    }

    #[cfg(feature = "kuromoji")]
    pub fn kuromoji_resources(&self) -> &crate::kuromoji::KuromojiResources {
        &self.0.kuromoji
    }

    pub fn limits(&self) -> AnalyzerLimits {
        self.0.limits
    }

    pub fn cache_stats(&self) -> AnalyzerCacheStats {
        let cache = self.0.cache.lock();
        AnalyzerCacheStats {
            analyzers: cache.len(),
            descriptor_bytes: cache.weight(),
        }
    }

    pub fn compile(&self, config: &Analyzer) -> AnalysisResult<Arc<CompiledAnalyzer>> {
        let policy = if config.uses_morphology_stages() {
            TokenLengthPolicy::DiscountOverlaps
        } else {
            TokenLengthPolicy::EmittedTokens
        };
        self.compile_with_length_policy(config, policy)
    }

    pub fn compile_with_length_policy(
        &self,
        config: &Analyzer,
        policy: TokenLengthPolicy,
    ) -> AnalysisResult<Arc<CompiledAnalyzer>> {
        // Mutable inputs resolve outside the cache lock, even when their previous revision is cached.
        let resolved = AnalyzerDescriptor::resolve_inputs(
            config,
            policy,
            self.0.limits,
            #[cfg(feature = "nori")]
            &self.0.nori,
            #[cfg(feature = "kuromoji")]
            &self.0.kuromoji,
        )?;
        self.publish(
            resolved.descriptor,
            #[cfg(feature = "nori")]
            resolved.nori,
            #[cfg(feature = "kuromoji")]
            resolved.kuromoji,
        )
    }

    /// Compile verified resolved inputs without consulting mutable files or named definitions.
    pub fn restore(
        &self,
        descriptor: Arc<AnalyzerDescriptor>,
    ) -> AnalysisResult<Arc<CompiledAnalyzer>> {
        descriptor.validate_limits(self.0.limits)?;
        if let Some(compiled) = self.0.cache.lock().get(&descriptor.fingerprint()) {
            return Ok(compiled);
        }
        #[cfg(any(feature = "nori", feature = "kuromoji"))]
        let mut config = descriptor.configuration()?;
        #[cfg(feature = "nori")]
        let nori = {
            crate::nori::pipeline::check_resolved(&config)?;
            crate::nori::pipeline::ResolvedNoriPipeline::resolve(&mut config, &self.0.nori)?
        };
        #[cfg(feature = "kuromoji")]
        let kuromoji = {
            crate::kuromoji::pipeline::check_resolved(&config)?;
            crate::kuromoji::pipeline::ResolvedKuromojiPipeline::resolve(
                &mut config,
                &self.0.kuromoji,
            )?
        };
        self.publish(
            descriptor,
            #[cfg(feature = "nori")]
            nori,
            #[cfg(feature = "kuromoji")]
            kuromoji,
        )
    }

    fn publish(
        &self,
        descriptor: Arc<AnalyzerDescriptor>,
        #[cfg(feature = "nori")] nori: crate::nori::pipeline::ResolvedNoriPipeline,
        #[cfg(feature = "kuromoji")] kuromoji: crate::kuromoji::pipeline::ResolvedKuromojiPipeline,
    ) -> AnalysisResult<Arc<CompiledAnalyzer>> {
        descriptor.validate_limits(self.0.limits)?;
        let fingerprint = descriptor.fingerprint();
        let weight = descriptor.canonical_json().len();
        let mut cache = self.0.cache.lock();
        if let Some(compiled) = cache.get(&fingerprint) {
            return Ok(compiled);
        }
        // Executable preparation receives resolved handles and cannot call external owners.
        let compiled = Arc::new(CompiledAnalyzer::prepare(
            descriptor,
            #[cfg(feature = "nori")]
            nori,
            #[cfg(feature = "kuromoji")]
            kuromoji,
        )?);
        cache.insert(
            fingerprint,
            compiled.clone(),
            weight,
            self.0.limits.max_cached_analyzers,
            self.0.limits.max_cached_descriptor_bytes,
        );
        Ok(compiled)
    }

    /// Validate the persisted fingerprint and runtime profiles before publishing a compiled handle.
    pub fn restore_json(&self, json: &str) -> AnalysisResult<Arc<CompiledAnalyzer>> {
        self.restore(AnalyzerDescriptor::from_json(json, self.0.limits)?)
    }
}
