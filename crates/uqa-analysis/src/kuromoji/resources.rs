//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Explicit immutable dictionary resolution and bounded content-based interning.

use std::sync::{Arc, OnceLock};

use serde::{Deserialize, Serialize};

use super::{DictionaryId, DictionaryResult, KuromojiDictionary, UserDictionary};

mod hash;

use crate::morphology::resources::{Artifact, Request, Resources};
pub use crate::morphology::resources::{DictionaryBytes, ResourceCacheStats, ResourceLimits};
pub use hash::ResourceHash;

pub const DEFAULT_KUROMOJI_DICTIONARY: &str = "lucene-10.5.1";

/// Names are resolved at compilation; persisted references use the exact artifact hash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DictionaryRequest {
    Name(String),
    Sha256(ResourceHash),
}

impl std::fmt::Display for DictionaryRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Name(name) => formatter.write_str(name),
            Self::Sha256(hash) => write!(formatter, "sha256:{hash}"),
        }
    }
}

/// Untrusted resolver output: both the declared hash and the bundle are checked before caching.
pub struct DictionaryArtifact {
    pub sha256: ResourceHash,
    pub bytes: DictionaryBytes,
}

/// Resolve explicit bytes without any implicit filesystem or network fallback.
pub trait DictionaryResolver: Send + Sync {
    fn resolve(&self, request: &DictionaryRequest) -> DictionaryResult<Option<DictionaryArtifact>>;
}

impl<F> DictionaryResolver for F
where
    F: Fn(&DictionaryRequest) -> DictionaryResult<Option<DictionaryArtifact>> + Send + Sync,
{
    fn resolve(&self, request: &DictionaryRequest) -> DictionaryResult<Option<DictionaryArtifact>> {
        self(request)
    }
}

/// An immutable validated model and the exact bytes required to resolve its artifact again.
pub struct ResolvedDictionary {
    sha256: ResourceHash,
    bytes: DictionaryBytes,
    model: Arc<KuromojiDictionary>,
}

impl std::fmt::Debug for ResolvedDictionary {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ResolvedDictionary")
            .field("sha256", &self.sha256)
            .field("model", &self.model)
            .finish_non_exhaustive()
    }
}

impl ResolvedDictionary {
    pub fn sha256(&self) -> ResourceHash {
        self.sha256
    }
    pub fn model(&self) -> &Arc<KuromojiDictionary> {
        &self.model
    }
    pub fn bytes(&self) -> &[u8] {
        self.bytes.as_ref()
    }
}

/// Exact source identity is retained even when comments/whitespace compile to no user entries.
pub struct ResolvedUserDictionary {
    sha256: ResourceHash,
    model_id: DictionaryId,
    dictionary: Option<Arc<UserDictionary>>,
    empty_source: Option<Arc<str>>,
}

impl ResolvedUserDictionary {
    pub fn sha256(&self) -> ResourceHash {
        self.sha256
    }
    pub fn model_id(&self) -> DictionaryId {
        self.model_id
    }
    pub fn dictionary(&self) -> Option<&Arc<UserDictionary>> {
        self.dictionary.as_ref()
    }
    pub fn source(&self) -> &str {
        match &self.dictionary {
            Some(dictionary) => dictionary.source(),
            None => self.empty_source.as_deref().expect("retained empty rules"),
        }
    }
}

struct Inner {
    resolver: Arc<dyn DictionaryResolver>,
    resources: Resources<ResolvedDictionary, ResolvedUserDictionary, DictionaryId>,
}

/// Cloneable resource ownership, independent of any analyzer name, session, or worker state.
///
/// ```
/// use uqa_analysis::kuromoji::{DictionaryRequest, KuromojiResources};
/// let resources = KuromojiResources::default();
/// let dictionary = resources.load_default()?;
/// let exact = resources.load(&DictionaryRequest::Sha256(dictionary.sha256()))?;
/// assert!(std::sync::Arc::ptr_eq(&dictionary, &exact));
/// let rules = resources.compile_user("東京大学,東京 大学,トウキョウ ダイガク,名詞", dictionary.model())?;
/// let user = rules.dictionary().unwrap();
/// let phrase = user.lookup("東京大学").unwrap();
/// assert_eq!(user.entry(phrase).unwrap().segment_lengths(), [2, 2]);
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Clone)]
pub struct KuromojiResources(Arc<Inner>);

impl Default for KuromojiResources {
    fn default() -> Self {
        static RESOURCES: OnceLock<KuromojiResources> = OnceLock::new();
        RESOURCES
            .get_or_init(|| {
                Self::with_resolver(Arc::new(BundledResolver), ResourceLimits::default())
            })
            .clone()
    }
}

impl KuromojiResources {
    pub fn with_resolver(resolver: Arc<dyn DictionaryResolver>, limits: ResourceLimits) -> Self {
        Self(Arc::new(Inner {
            resolver,
            resources: Resources::new(limits),
        }))
    }

    pub fn limits(&self) -> ResourceLimits {
        self.0.resources.limits()
    }

    pub fn cache_stats(&self) -> ResourceCacheStats {
        self.0.resources.stats()
    }

    pub fn load_default(&self) -> DictionaryResult<Arc<ResolvedDictionary>> {
        self.load(&DictionaryRequest::Name(DEFAULT_KUROMOJI_DICTIONARY.into()))
    }

    /// Load immutable content, validating declared/requested hashes before publishing a cache entry.
    ///
    /// A cached exact hash resolves without calling the host again. Names always consult the resolver so changing an alias cannot mutate an existing handle or disguise a new revision.
    pub fn load(&self, request: &DictionaryRequest) -> DictionaryResult<Arc<ResolvedDictionary>> {
        let shared_request = match request {
            DictionaryRequest::Name(name) => Request::Name(name),
            DictionaryRequest::Sha256(hash) => Request::Sha256(hash.into_bytes()),
        };
        self.0.resources.load(
            shared_request,
            || {
                self.0.resolver.resolve(request).map(|artifact| {
                    artifact.map(|artifact| Artifact {
                        sha256: artifact.sha256.into_bytes(),
                        bytes: artifact.bytes,
                    })
                })
            },
            |artifact| {
                let model = KuromojiDictionary::from_bytes(
                    artifact.bytes.as_ref(),
                    self.limits().dictionary,
                )?;
                Ok(ResolvedDictionary {
                    sha256: ResourceHash::from_bytes(artifact.sha256),
                    bytes: artifact.bytes,
                    model,
                })
            },
        )
    }

    /// Intern exact rule source against the model's semantic identity; failed compilation is not cached.
    pub fn compile_user(
        &self,
        source: &str,
        model: &KuromojiDictionary,
    ) -> DictionaryResult<Arc<ResolvedUserDictionary>> {
        self.0.resources.compile_user(source, model.id(), |hash| {
            let dictionary = UserDictionary::compile(source, model, self.limits().user_dictionary)?;
            let empty_source = dictionary.is_none().then(|| Arc::from(source));
            Ok(ResolvedUserDictionary {
                sha256: ResourceHash::from_bytes(hash),
                model_id: model.id(),
                dictionary,
                empty_source,
            })
        })
    }
}

struct BundledResolver;

impl DictionaryResolver for BundledResolver {
    fn resolve(&self, request: &DictionaryRequest) -> DictionaryResult<Option<DictionaryArtifact>> {
        let hash = uqa_kuromoji_data::BUNDLE_SHA256.parse()?;
        let matches = match request {
            DictionaryRequest::Name(name) => name == DEFAULT_KUROMOJI_DICTIONARY,
            DictionaryRequest::Sha256(expected) => *expected == hash,
        };
        Ok(matches.then_some(DictionaryArtifact {
            sha256: hash,
            bytes: DictionaryBytes::Static(uqa_kuromoji_data::BUNDLE),
        }))
    }
}

#[cfg(test)]
mod tests;
