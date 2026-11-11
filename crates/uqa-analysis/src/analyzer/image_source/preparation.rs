//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordinary cache ownership and unpublished image ownership share the same encoded constructor.

use super::{CompiledAnalyzer, ImageData};
use crate::work::Work;
use std::sync::Arc;
use uqa_core::{
    image_io::{AsyncImageInput, ImageIoError},
    memory::{MemoryError, Produced, ProductionControl, ProductionString},
    ValueRetentionError,
};

#[cfg(any(feature = "nori", feature = "kuromoji"))]
use super::{AnalyzerResourceKind, DictionaryImage, ImageResource};
#[cfg(any(feature = "nori", feature = "kuromoji"))]
use uqa_core::memory::ProductionVec;

pub(super) struct Retained<'a, 'b>(pub &'a mut AsyncImageInput<'b>);

impl Work<ValueRetentionError> for Retained<'_, '_> {
    async fn checkpoint(&mut self) -> Result<(), ValueRetentionError> {
        self.0.checkpoint().await.map_err(|error| match error {
            ImageIoError::Memory(error) => error.into(),
            ImageIoError::Cancelled(error) => error.into(),
            error => ValueRetentionError::Malformed {
                kind: "analyzer image source continuation",
                reason: error.to_string(),
            },
        })
    }
}

pub(super) trait Sources: Work<ValueRetentionError> {
    #[cfg(feature = "nori")]
    fn nori(
        &mut self,
        dictionary: &crate::nori::ResolvedDictionary,
        control: &ProductionControl<'_>,
    ) -> impl std::future::Future<Output = Result<DictionaryImage, ValueRetentionError>> + Send;
    #[cfg(feature = "kuromoji")]
    fn kuromoji(
        &mut self,
        dictionary: &crate::kuromoji::ResolvedDictionary,
        control: &ProductionControl<'_>,
    ) -> impl std::future::Future<Output = Result<DictionaryImage, ValueRetentionError>> + Send;
}

impl Sources for () {
    #[cfg(feature = "nori")]
    async fn nori(
        &mut self,
        dictionary: &crate::nori::ResolvedDictionary,
        control: &ProductionControl<'_>,
    ) -> Result<DictionaryImage, ValueRetentionError> {
        dictionary.image_source(control)
    }
    #[cfg(feature = "kuromoji")]
    async fn kuromoji(
        &mut self,
        dictionary: &crate::kuromoji::ResolvedDictionary,
        control: &ProductionControl<'_>,
    ) -> Result<DictionaryImage, ValueRetentionError> {
        dictionary.image_source(control)
    }
}

impl Sources for Retained<'_, '_> {
    #[cfg(feature = "nori")]
    async fn nori(
        &mut self,
        dictionary: &crate::nori::ResolvedDictionary,
        control: &ProductionControl<'_>,
    ) -> Result<DictionaryImage, ValueRetentionError> {
        dictionary.image_source_with_work(control, self).await
    }
    #[cfg(feature = "kuromoji")]
    async fn kuromoji(
        &mut self,
        dictionary: &crate::kuromoji::ResolvedDictionary,
        control: &ProductionControl<'_>,
    ) -> Result<DictionaryImage, ValueRetentionError> {
        dictionary.image_source_with_work(control, self).await
    }
}

pub(super) async fn prepare(
    analyzer: &CompiledAnalyzer,
    control: &ProductionControl<'_>,
    work: &mut impl Sources,
) -> Result<Arc<Produced<ImageData>>, ValueRetentionError> {
    control.check()?;
    let text = analyzer.descriptor().canonical_json();
    let mut descriptor = ProductionString::new(*control);
    descriptor.reserve(text.len())?;
    let mut position = 0;
    while position < text.len() {
        work.checkpoint().await?;
        let part = crate::json::fragment(text, position);
        descriptor.push_str(part)?;
        position += part.len();
    }
    let descriptor = descriptor.finish()?;
    #[cfg(any(feature = "nori", feature = "kuromoji"))]
    let (resources, resource_bytes) = {
        let mut resources = ProductionVec::<ImageResource>::new(*control);
        resources.reserve(analyzer.dictionary_resources().count())?;
        #[cfg(feature = "nori")]
        for dictionary in &analyzer.nori_dictionaries {
            work.checkpoint().await?;
            let image = work.nori(dictionary, control).await?;
            resources.push_produced(control.finish(
                ImageResource {
                    kind: AnalyzerResourceKind::Nori,
                    sha256: dictionary.sha256().into_bytes(),
                    image,
                },
                control.empty_reservation(),
            )?)?;
        }
        #[cfg(feature = "kuromoji")]
        for dictionary in &analyzer.kuromoji_dictionaries {
            work.checkpoint().await?;
            let image = work.kuromoji(dictionary, control).await?;
            resources.push_produced(control.finish(
                ImageResource {
                    kind: AnalyzerResourceKind::Kuromoji,
                    sha256: dictionary.sha256().into_bytes(),
                    image,
                },
                control.empty_reservation(),
            )?)?;
        }
        let resources = resources.finish()?;
        let mut bytes = resources
            .capacity()
            .checked_mul(size_of::<ImageResource>())
            .ok_or(MemoryError::SizeOverflow)?;
        for resource in resources.iter() {
            work.checkpoint().await?;
            bytes = bytes
                .checked_add(resource.image.retained_bytes())
                .ok_or(MemoryError::SizeOverflow)?;
        }
        (resources, bytes)
    };
    #[cfg(not(any(feature = "nori", feature = "kuromoji")))]
    let resource_bytes = 0;
    let retained_bytes = size_of::<Produced<ImageData>>()
        .checked_add(descriptor.capacity())
        .and_then(|bytes| bytes.checked_add(resource_bytes))
        .ok_or(MemoryError::SizeOverflow)?;
    let (descriptor, descriptor_memory) = descriptor.into_parts();
    #[cfg(any(feature = "nori", feature = "kuromoji"))]
    let (resources, resource_memory) = resources.into_parts();
    #[cfg(not(any(feature = "nori", feature = "kuromoji")))]
    let resource_memory = control.empty_reservation();
    control
        .finish(
            ImageData {
                descriptor,
                #[cfg(any(feature = "nori", feature = "kuromoji"))]
                resources,
                retained_bytes,
            },
            control.combine(descriptor_memory, resource_memory),
        )?
        .into_shared(control)
}
