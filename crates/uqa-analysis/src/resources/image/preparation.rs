//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Image sources validate requested artifacts lazily and never enter live resource caches.

use super::{AnalyzerResourceKind, ImageResources};
use crate::morphology::{
    resources::{validate_artifact, Artifact, Error, Request, Source},
    work::Work,
};
use std::sync::Arc;

impl ImageResources<'_> {
    fn artifact(
        &self,
        kind: AnalyzerResourceKind,
        request: Request<'_>,
    ) -> Result<Artifact, Error> {
        let input = match request {
            Request::Sha256(hash) => self.find(kind, hash),
            Request::Name(_) => None,
        }
        .ok_or_else(|| Error::Missing(request.to_string()))?;
        Ok(Artifact {
            sha256: input.sha256,
            bytes: crate::morphology::resources::DictionaryBytes::Retained(input.bytes.clone()),
        })
    }
}

#[cfg(feature = "nori")]
impl Source<crate::nori::DictionaryRequest, crate::nori::ResolvedDictionary>
    for ImageResources<'_>
{
    type Error = crate::nori::DictionaryError;
    async fn load(
        &self,
        request: &crate::nori::DictionaryRequest,
        work: &mut impl Work,
    ) -> crate::nori::DictionaryResult<Arc<crate::nori::ResolvedDictionary>> {
        let request = match request {
            crate::nori::DictionaryRequest::Name(name) => Request::Name(name),
            crate::nori::DictionaryRequest::Sha256(hash) => Request::Sha256(hash.into_bytes()),
        };
        let artifact = self.artifact(AnalyzerResourceKind::Nori, request)?;
        let limits = self.owner.nori_resources().limits().dictionary;
        validate_artifact(request, &artifact, limits, work).await?;
        Ok(Arc::new(
            crate::nori::ResolvedDictionary::from_artifact_with(artifact, limits, work).await?,
        ))
    }
}

#[cfg(feature = "nori")]
impl crate::nori::pipeline::Source for ImageResources<'_> {
    async fn compile_user(
        &self,
        source: &str,
        model: &crate::nori::NoriDictionary,
        work: &mut impl Work,
    ) -> crate::nori::DictionaryResult<Option<Arc<crate::nori::UserDictionary>>> {
        crate::nori::UserDictionary::compile_with(
            source,
            model,
            self.owner.nori_resources().limits().user_dictionary,
            work,
        )
        .await
    }
}

#[cfg(feature = "kuromoji")]
impl Source<crate::kuromoji::DictionaryRequest, crate::kuromoji::ResolvedDictionary>
    for ImageResources<'_>
{
    type Error = crate::kuromoji::DictionaryError;
    async fn load(
        &self,
        request: &crate::kuromoji::DictionaryRequest,
        work: &mut impl Work,
    ) -> crate::kuromoji::DictionaryResult<Arc<crate::kuromoji::ResolvedDictionary>> {
        let request = match request {
            crate::kuromoji::DictionaryRequest::Name(name) => Request::Name(name),
            crate::kuromoji::DictionaryRequest::Sha256(hash) => Request::Sha256(hash.into_bytes()),
        };
        let artifact = self.artifact(AnalyzerResourceKind::Kuromoji, request)?;
        let limits = self.owner.kuromoji_resources().limits().dictionary;
        validate_artifact(request, &artifact, limits, work).await?;
        Ok(Arc::new(
            crate::kuromoji::ResolvedDictionary::from_artifact_with(artifact, limits, work).await?,
        ))
    }
}

#[cfg(feature = "kuromoji")]
impl crate::kuromoji::pipeline::Source for ImageResources<'_> {
    async fn compile_user(
        &self,
        source: &str,
        model: &crate::kuromoji::KuromojiDictionary,
        work: &mut impl Work,
    ) -> crate::kuromoji::DictionaryResult<Option<Arc<crate::kuromoji::UserDictionary>>> {
        crate::kuromoji::UserDictionary::compile_with(
            source,
            model,
            self.owner.kuromoji_resources().limits().user_dictionary,
            work,
        )
        .await
    }
}
