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
    memory::{MemoryError, Produced, ProductionControl},
    ValueRetentionError,
};

#[cfg(any(feature = "nori", feature = "kuromoji"))]
use uqa_core::memory::ProductionVec;

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
        let (descriptor, descriptor_memory) = control
            .copy_text(self.descriptor().canonical_json())?
            .into_parts();
        #[cfg(any(feature = "nori", feature = "kuromoji"))]
        let (resources, resource_memory, resource_bytes) = {
            let mut resources = ProductionVec::<ImageResource>::new(*control);
            resources.reserve(self.dictionary_resources().count())?;
            #[cfg(feature = "nori")]
            for dictionary in &self.nori_dictionaries {
                resources.push_produced(control.finish(
                    ImageResource {
                        kind: AnalyzerResourceKind::Nori,
                        sha256: dictionary.sha256().into_bytes(),
                        image: dictionary.image_source(control)?,
                    },
                    control.empty_reservation(),
                )?)?;
            }
            #[cfg(feature = "kuromoji")]
            for dictionary in &self.kuromoji_dictionaries {
                resources.push_produced(control.finish(
                    ImageResource {
                        kind: AnalyzerResourceKind::Kuromoji,
                        sha256: dictionary.sha256().into_bytes(),
                        image: dictionary.image_source(control)?,
                    },
                    control.empty_reservation(),
                )?)?;
            }
            let (resources, resource_memory) = resources.finish()?.into_parts();
            let resource_bytes = resources.iter().try_fold(
                resources
                    .capacity()
                    .checked_mul(size_of::<ImageResource>())
                    .ok_or(MemoryError::SizeOverflow)?,
                |bytes, resource| {
                    bytes
                        .checked_add(resource.image.retained_bytes())
                        .ok_or(MemoryError::SizeOverflow)
                },
            )?;
            (resources, resource_memory, resource_bytes)
        };
        #[cfg(not(any(feature = "nori", feature = "kuromoji")))]
        let (resource_memory, resource_bytes) = (control.empty_reservation(), 0);
        let retained_bytes = size_of::<Produced<ImageData>>()
            .checked_add(descriptor.capacity())
            .and_then(|bytes| bytes.checked_add(resource_bytes))
            .ok_or(MemoryError::SizeOverflow)?;
        let source = control
            .finish(
                ImageData {
                    descriptor,
                    #[cfg(any(feature = "nori", feature = "kuromoji"))]
                    resources,
                    retained_bytes,
                },
                control.combine(descriptor_memory, resource_memory),
            )?
            .into_shared(control)?;
        *cached = Arc::downgrade(&source);
        Ok(AnalyzerImageSource(source))
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
