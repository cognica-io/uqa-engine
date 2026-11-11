//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Typed pipeline sources preserve ordinary cache ownership and retained image construction.

use super::{
    Artifact, DictionaryRequest, DictionaryResult, NoriDictionary, NoriResources,
    ResolvedDictionary, ResourceHash, UserDictionary,
};
use crate::morphology::{resources::Source as DictionarySource, work::Work};
use std::sync::Arc;

pub(crate) trait Source:
    DictionarySource<DictionaryRequest, ResolvedDictionary, Error = super::super::DictionaryError>
{
    fn compile_user(
        &self,
        source: &str,
        model: &NoriDictionary,
        work: &mut impl Work,
    ) -> impl std::future::Future<Output = DictionaryResult<Option<Arc<UserDictionary>>>> + Send;
}

impl DictionarySource<DictionaryRequest, ResolvedDictionary> for NoriResources {
    type Error = super::super::DictionaryError;

    async fn load(
        &self,
        request: &DictionaryRequest,
        _work: &mut impl Work,
    ) -> DictionaryResult<Arc<ResolvedDictionary>> {
        self.load(request)
    }
}

impl Source for NoriResources {
    async fn compile_user(
        &self,
        source: &str,
        model: &NoriDictionary,
        _work: &mut impl Work,
    ) -> DictionaryResult<Option<Arc<UserDictionary>>> {
        Ok(self.compile_user(source, model)?.dictionary().cloned())
    }
}

impl ResolvedDictionary {
    pub(crate) async fn from_artifact_with(
        artifact: Artifact,
        limits: super::super::DictionaryLimits,
        work: &mut impl Work,
    ) -> DictionaryResult<Self> {
        let model = NoriDictionary::from_bytes_with(artifact.bytes.as_ref(), limits, work).await?;
        Ok(Self {
            image: crate::analyzer::image_source::DictionaryImageCache::default(),
            sha256: ResourceHash::from_bytes(artifact.sha256),
            bytes: artifact.bytes,
            model,
        })
    }
}
