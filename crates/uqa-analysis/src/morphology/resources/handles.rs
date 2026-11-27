//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Typed resolver requests and immutable handles for validated morphology resources.

use super::DictionaryBytes;
use crate::analyzer::image_source::{DictionaryImage, DictionaryImageCache};
use std::sync::Arc;
use uqa_core::memory::ProductionControl;
use uqa_core::ValueRetentionError;

/// Untrusted resolver output. The declared hash and bundle are checked before caching.
pub struct DictionaryArtifact<H> {
    pub sha256: H,
    pub bytes: DictionaryBytes,
}

impl<H> DictionaryArtifact<H> {
    pub(crate) fn map_hash<T>(self, convert: impl FnOnce(H) -> T) -> DictionaryArtifact<T> {
        DictionaryArtifact {
            sha256: convert(self.sha256),
            bytes: self.bytes,
        }
    }
}

/// An immutable validated model and the exact bytes required to resolve its artifact again.
pub struct ResolvedDictionary<M, H> {
    image: DictionaryImageCache,
    sha256: H,
    bytes: DictionaryBytes,
    model: Arc<M>,
}

impl<M: std::fmt::Debug, H: std::fmt::Debug> std::fmt::Debug for ResolvedDictionary<M, H> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ResolvedDictionary")
            .field("sha256", &self.sha256)
            .field("model", &self.model)
            .finish_non_exhaustive()
    }
}

impl<M, H: Copy> ResolvedDictionary<M, H> {
    pub(crate) fn new(model: Arc<M>, sha256: H, bytes: DictionaryBytes) -> Self {
        Self {
            image: DictionaryImageCache::default(),
            sha256,
            bytes,
            model,
        }
    }
    pub(crate) fn image_source(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<DictionaryImage, ValueRetentionError> {
        self.image.prepare(&self.bytes, control)
    }
    pub(crate) async fn image_source_with_work(
        &self,
        control: &ProductionControl<'_>,
        work: &mut impl crate::work::Work<ValueRetentionError>,
    ) -> Result<DictionaryImage, ValueRetentionError> {
        DictionaryImage::prepare_with(&self.bytes, control, work).await
    }
    pub fn sha256(&self) -> H {
        self.sha256
    }
    pub fn model(&self) -> &Arc<M> {
        &self.model
    }
    pub fn bytes(&self) -> &[u8] {
        self.bytes.as_ref()
    }
}

enum UserRules<U> {
    Compiled(Arc<U>),
    Empty(Arc<str>),
}

/// Exact source identity is retained even when comments or whitespace produce no user entries.
pub struct ResolvedUserDictionary<U, I, H> {
    sha256: H,
    model_id: I,
    rules: UserRules<U>,
}

impl<U, I: Copy, H: Copy> ResolvedUserDictionary<U, I, H> {
    pub(crate) fn new(sha256: H, model_id: I, dictionary: Option<Arc<U>>, source: &str) -> Self {
        let rules =
            dictionary.map_or_else(|| UserRules::Empty(Arc::from(source)), UserRules::Compiled);
        Self {
            sha256,
            model_id,
            rules,
        }
    }
    pub fn sha256(&self) -> H {
        self.sha256
    }
    pub fn model_id(&self) -> I {
        self.model_id
    }
    pub fn dictionary(&self) -> Option<&Arc<U>> {
        match &self.rules {
            UserRules::Compiled(dictionary) => Some(dictionary),
            UserRules::Empty(_) => None,
        }
    }
    pub(crate) fn rule_source(&self, source: impl FnOnce(&U) -> &str) -> &str {
        match &self.rules {
            UserRules::Compiled(dictionary) => source(dictionary),
            UserRules::Empty(text) => text,
        }
    }
}

// The public enum variants and resolver trait can be imported or implemented by
// downstream crates. Each language retains its concrete hash and error types.
macro_rules! dictionary_resolver {
    ($hash:ty, $error:ty) => {
        /// Names are resolved at compilation. Persisted references use the exact artifact hash.
        #[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
        #[serde(rename_all = "snake_case")]
        pub enum DictionaryRequest {
            Name(String),
            Sha256($hash),
        }

        impl DictionaryRequest {
            pub(crate) fn as_request(&self) -> $crate::morphology::resources::Request<'_> {
                use $crate::morphology::resources::Request;
                match self {
                    Self::Name(name) => Request::Name(name),
                    Self::Sha256(hash) => Request::Sha256(hash.into_bytes()),
                }
            }
        }

        impl std::fmt::Display for DictionaryRequest {
            fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                std::fmt::Display::fmt(&self.as_request(), formatter)
            }
        }

        pub type DictionaryArtifact = $crate::morphology::resources::handles::DictionaryArtifact<$hash>;

        /// Resolve explicit dictionary bytes supplied by the host.
        pub trait DictionaryResolver: Send + Sync {
            fn resolve(&self, request: &DictionaryRequest) -> Result<Option<DictionaryArtifact>, $error>;
        }

        impl<F: Fn(&DictionaryRequest) -> Result<Option<DictionaryArtifact>, $error> + Send + Sync> DictionaryResolver for F {
            fn resolve(&self, request: &DictionaryRequest) -> Result<Option<DictionaryArtifact>, $error> {
                self(request)
            }
        }
    };
}
pub(crate) use dictionary_resolver;
