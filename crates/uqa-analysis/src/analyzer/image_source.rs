//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Publication owns encoded analyzer inputs independently of executable pipelines.

#[cfg(any(feature = "nori", feature = "kuromoji"))]
use super::AnalyzerResourceKind;
use super::{AnalyzerResource, CompiledAnalyzer};
use std::sync::Arc;
use uqa_core::{
    memory::{Produced, ProductionControl},
    ValueRetentionError,
};

mod cache;
mod preparation;

use cache::{ImageCache, SharedImage};

#[cfg(any(feature = "nori", feature = "kuromoji"))]
mod dictionary;
#[cfg(any(feature = "nori", feature = "kuromoji"))]
pub(crate) use dictionary::DictionaryImage;
#[cfg(any(feature = "nori", feature = "kuromoji"))]
pub(crate) use dictionary::DictionaryImageCache;

pub(super) type AnalyzerImageCache = ImageCache<ImageData>;

#[cfg(any(feature = "nori", feature = "kuromoji"))]
#[derive(Debug)]
struct ImageResource {
    kind: AnalyzerResourceKind,
    sha256: [u8; 32],
    image: DictionaryImage,
}

#[derive(Debug)]
pub(super) struct ImageData {
    descriptor: String,
    #[cfg(any(feature = "nori", feature = "kuromoji"))]
    resources: Vec<ImageResource>,
    retained_bytes: usize,
}

/// Immutable wire inputs prepared before native catalog publication. This owner retains neither a compiled pipeline nor decoded dictionary models or an imported input allowance.
#[derive(Debug, Clone)]
pub struct AnalyzerImageSource(SharedImage<ImageData>);

impl CompiledAnalyzer {
    /// Reuse the live encoded owner for this construction allowance. Revisions sharing a dictionary also share its encoded owner within the same allowance.
    pub fn image_source_with_control(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<AnalyzerImageSource, ValueRetentionError> {
        self.image_source
            .get_or_prepare(control, || {
                crate::work::inline(preparation::prepare(self, control, &mut ()))
            })
            .map(AnalyzerImageSource)
    }
}

impl CompiledAnalyzer {
    /// Prepare private encoded ownership from an existing immutable revision through retained work. The revision may belong to a process-shared compilation cache, so restoration neither adopts its cached image nor publishes request-owned allocations into that cache. Ordinary readers cannot retain the private candidate's allowance.
    pub async fn image_source_with_input(
        &self,
        input: &mut uqa_core::image_io::AsyncImageInput<'_>,
    ) -> Result<AnalyzerImageSource, ValueRetentionError> {
        let control = ProductionControl::new(input.memory, input.cancellation, input.cancellation);
        control.check()?;
        let source =
            preparation::prepare(self, &control, &mut preparation::Retained(input)).await?;
        control.check()?;
        Ok(AnalyzerImageSource(source))
    }

    /// Prepare independent encoded catalog ownership before sharing a freshly reconstructed revision. Consuming the private revision keeps its caches inaccessible throughout suspension; the returned image owns its copied descriptor/artifacts independently of imported allocations and executable models.
    pub async fn into_image_source_with_input(
        mut self,
        input: &mut uqa_core::image_io::AsyncImageInput<'_>,
    ) -> Result<(Arc<Self>, AnalyzerImageSource), ValueRetentionError> {
        let control = ProductionControl::new(input.memory, input.cancellation, input.cancellation);
        let source =
            preparation::prepare(&self, &control, &mut preparation::Retained(input)).await?;
        let source = self.image_source.publish_prepared(&control, source)?;
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
