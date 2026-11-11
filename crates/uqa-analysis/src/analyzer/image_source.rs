//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Publication owns encoded analyzer inputs independently of executable pipelines.

#[cfg(any(feature = "nori", feature = "kuromoji"))]
use super::AnalyzerResourceKind;
use super::{AnalyzerResource, CompiledAnalyzer};
use parking_lot::Mutex;
use std::sync::{Arc, Weak};
use uqa_core::{
    memory::{Produced, ProductionControl},
    ValueRetentionError,
};

mod preparation;

#[cfg(any(feature = "nori", feature = "kuromoji"))]
mod dictionary;
#[cfg(any(feature = "nori", feature = "kuromoji"))]
pub(crate) use dictionary::DictionaryImage;
#[cfg(any(feature = "nori", feature = "kuromoji"))]
pub(crate) use dictionary::DictionaryImageCache;

#[derive(Debug, Default)]
pub(super) struct AnalyzerImageCache(Mutex<Weak<Produced<ImageData>>>);

#[cfg(any(feature = "nori", feature = "kuromoji"))]
#[derive(Debug)]
struct ImageResource {
    kind: AnalyzerResourceKind,
    sha256: [u8; 32],
    image: DictionaryImage,
}

#[derive(Debug)]
struct ImageData {
    descriptor: String,
    #[cfg(any(feature = "nori", feature = "kuromoji"))]
    resources: Vec<ImageResource>,
    retained_bytes: usize,
}

/// Immutable wire inputs prepared before native catalog publication. This owner retains neither a compiled pipeline nor decoded dictionary models or an imported input allowance.
#[derive(Debug, Clone)]
pub struct AnalyzerImageSource(Arc<Produced<ImageData>>);

impl CompiledAnalyzer {
    /// Prepare this revision's encoded owner once while it remains in use. Dictionary owners are shared across revisions of the same resolved artifact; the cache itself retains only weak references.
    pub fn image_source_with_control(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<AnalyzerImageSource, ValueRetentionError> {
        control.check()?;
        let mut cached = self.image_source.0.lock();
        control.check()?;
        if let Some(source) = cached.upgrade() {
            return Ok(AnalyzerImageSource(source));
        }
        let source = crate::work::inline(preparation::prepare(self, control, &mut ()))?;
        *cached = Arc::downgrade(&source);
        Ok(AnalyzerImageSource(source))
    }
}

impl CompiledAnalyzer {
    /// Prepare independent encoded catalog ownership before sharing a freshly reconstructed revision. Consuming the private revision keeps its caches inaccessible throughout suspension; the returned image owns its copied descriptor/artifacts independently of imported allocations and executable models.
    pub async fn into_image_source_with_input(
        mut self,
        input: &mut uqa_core::image_io::AsyncImageInput<'_>,
    ) -> Result<(Arc<Self>, AnalyzerImageSource), ValueRetentionError> {
        let control = ProductionControl::new(input.memory, input.cancellation, input.cancellation);
        let source =
            preparation::prepare(&self, &control, &mut preparation::Retained(input)).await?;
        *self.image_source.0.get_mut() = Arc::downgrade(&source);
        Ok((Arc::new(self), AnalyzerImageSource(source)))
    }
}

impl AnalyzerImageSource {
    pub fn canonical_json(&self) -> &str {
        &self.0.descriptor
    }

    pub fn dictionary_resources(&self) -> impl ExactSizeIterator<Item = AnalyzerResource<'_>> {
        #[cfg(any(feature = "nori", feature = "kuromoji"))]
        {
            self.0.resources.iter().map(|resource| AnalyzerResource {
                kind: resource.kind,
                sha256: resource.sha256,
                bytes: resource.image.bytes(),
            })
        }
        #[cfg(not(any(feature = "nori", feature = "kuromoji")))]
        {
            std::iter::empty()
        }
    }

    /// Complete encoded closure, including retained shared/static dictionary bytes. Reference-count bookkeeping is excluded, as for other native immutable owners.
    pub fn retained_bytes(&self) -> usize {
        self.0.retained_bytes
    }

    /// Independent admission uses the cached footprint and shares the immutable owner without visiting names, descriptors or dictionary bytes.
    pub fn capture(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Self>, ValueRetentionError> {
        let memory = control.reserve(self.retained_bytes())?;
        control.finish(self.clone(), memory)
    }
}

#[cfg(test)]
mod tests;
