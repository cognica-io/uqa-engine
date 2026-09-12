//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Explicit immutable dictionary resolution and bounded content-based interning.

use std::sync::{Arc, OnceLock};

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use super::error::check_limit;
use super::{
    DictionaryError, DictionaryId, DictionaryLimits, DictionaryResult, NoriDictionary,
    UserDictionary, UserDictionaryLimits,
};

mod cache;
mod hash;

use cache::Cache;
pub use hash::ResourceHash;

pub const DEFAULT_NORI_DICTIONARY: &str = "lucene-10.5.1";

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

/// Static bundles need no copied byte buffer; host-provided buffers share their allocation.
#[derive(Clone)]
pub enum DictionaryBytes {
    Static(&'static [u8]),
    Shared(Arc<[u8]>),
}

impl AsRef<[u8]> for DictionaryBytes {
    fn as_ref(&self) -> &[u8] {
        match self {
            Self::Static(bytes) => bytes,
            Self::Shared(bytes) => bytes,
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

#[derive(Debug, Clone, Copy)]
pub struct ResourceLimits {
    /// Maximum strong references retained by the dictionary cache; zero disables retention.
    pub max_cached_dictionaries: usize,
    /// Sum of encoded bundle sizes retained by the cache, including static bundle references.
    pub max_cached_encoded_bytes: usize,
    /// Maximum compiled user-rule snapshots retained; zero disables retention.
    pub max_cached_user_dictionaries: usize,
    /// Sum of exact UTF-8 rule source sizes retained by the cache.
    pub max_cached_user_source_bytes: usize,
    pub dictionary: DictionaryLimits,
    pub user_dictionary: UserDictionaryLimits,
}

impl Default for ResourceLimits {
    fn default() -> Self {
        Self {
            max_cached_dictionaries: 2,
            max_cached_encoded_bytes: 32 * 1024 * 1024,
            max_cached_user_dictionaries: 64,
            max_cached_user_source_bytes: 8 * 1024 * 1024,
            dictionary: DictionaryLimits::default(),
            user_dictionary: UserDictionaryLimits::default(),
        }
    }
}

/// An immutable validated model and the exact bytes required to resolve its artifact again.
pub struct ResolvedDictionary {
    sha256: ResourceHash,
    bytes: DictionaryBytes,
    model: Arc<NoriDictionary>,
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
    pub fn model(&self) -> &Arc<NoriDictionary> {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResourceCacheStats {
    pub dictionaries: usize,
    pub dictionary_encoded_bytes: usize,
    pub user_dictionaries: usize,
    pub user_source_bytes: usize,
}

#[derive(Default)]
struct State {
    dictionaries: Cache<ResourceHash, ResolvedDictionary>,
    users: Cache<(DictionaryId, ResourceHash), ResolvedUserDictionary>,
}

struct Inner {
    resolver: Arc<dyn DictionaryResolver>,
    limits: ResourceLimits,
    state: Mutex<State>,
}

/// Cloneable resource ownership, independent of any analyzer name, session, or worker state.
///
/// ```
/// use uqa_analysis::nori::{DictionaryRequest, KoreanTokenizer, NoriOptions, NoriResources};
/// let resources = NoriResources::default();
/// let dictionary = resources.load_default()?;
/// let exact = resources.load(&DictionaryRequest::Sha256(dictionary.sha256()))?;
/// assert!(std::sync::Arc::ptr_eq(&dictionary, &exact));
/// let rules = resources.compile_user("세종시 세종 시\n", dictionary.model())?;
/// let tokenizer = KoreanTokenizer::new(dictionary.model().clone(), rules.dictionary().cloned(), NoriOptions::default())?;
/// let tokens = tokenizer.tokenize("세종시")?;
/// assert_eq!(String::from_utf16(&tokens.tokens[0].term_utf16)?, "세종");
/// assert_eq!(String::from_utf16(&tokens.tokens[1].term_utf16)?, "시");
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Clone)]
pub struct NoriResources(Arc<Inner>);

impl Default for NoriResources {
    fn default() -> Self {
        static RESOURCES: OnceLock<NoriResources> = OnceLock::new();
        RESOURCES
            .get_or_init(|| {
                Self::with_resolver(Arc::new(BundledResolver), ResourceLimits::default())
            })
            .clone()
    }
}

impl NoriResources {
    pub fn with_resolver(resolver: Arc<dyn DictionaryResolver>, limits: ResourceLimits) -> Self {
        Self(Arc::new(Inner {
            resolver,
            limits,
            state: Mutex::new(State::default()),
        }))
    }

    pub fn limits(&self) -> ResourceLimits {
        self.0.limits
    }

    pub fn cache_stats(&self) -> ResourceCacheStats {
        let state = self.0.state.lock();
        ResourceCacheStats {
            dictionaries: state.dictionaries.len(),
            dictionary_encoded_bytes: state.dictionaries.weight(),
            user_dictionaries: state.users.len(),
            user_source_bytes: state.users.weight(),
        }
    }

    pub fn load_default(&self) -> DictionaryResult<Arc<ResolvedDictionary>> {
        self.load(&DictionaryRequest::Name(DEFAULT_NORI_DICTIONARY.into()))
    }

    /// Load immutable content, validating declared/requested hashes before publishing a cache entry.
    ///
    /// A cached exact hash resolves without calling the host again. Names always consult the resolver so changing an alias cannot mutate an existing handle or disguise a new revision.
    pub fn load(&self, request: &DictionaryRequest) -> DictionaryResult<Arc<ResolvedDictionary>> {
        if let DictionaryRequest::Sha256(hash) = request {
            if let Some(cached) = self.0.state.lock().dictionaries.get(hash) {
                return Ok(cached);
            }
        }
        // Host callbacks run outside the cache lock and may resolve other resources themselves.
        let artifact = self
            .0
            .resolver
            .resolve(request)?
            .ok_or_else(|| DictionaryError::ResourceMissing(request.to_string()))?;
        check_limit(
            "encoded bytes",
            artifact.bytes.as_ref().len(),
            self.0.limits.dictionary.max_encoded_bytes,
        )?;
        let hash = ResourceHash::of(artifact.bytes.as_ref());
        verify_hash(artifact.sha256, hash)?;
        if let DictionaryRequest::Sha256(expected) = request {
            verify_hash(*expected, hash)?;
        }
        let mut state = self.0.state.lock();
        if let Some(cached) = state.dictionaries.get(&hash) {
            return Ok(cached);
        }
        // Serialize decoding/publication so simultaneous misses share one validated allocation.
        let model = NoriDictionary::from_bytes(artifact.bytes.as_ref(), self.0.limits.dictionary)?;
        let resolved = Arc::new(ResolvedDictionary {
            sha256: hash,
            bytes: artifact.bytes,
            model,
        });
        state.dictionaries.insert(
            hash,
            resolved.clone(),
            resolved.bytes().len(),
            self.0.limits.max_cached_dictionaries,
            self.0.limits.max_cached_encoded_bytes,
        );
        Ok(resolved)
    }

    /// Intern exact rule source against the model's semantic identity; failed compilation is not cached.
    pub fn compile_user(
        &self,
        source: &str,
        model: &NoriDictionary,
    ) -> DictionaryResult<Arc<ResolvedUserDictionary>> {
        check_limit(
            "user dictionary bytes",
            source.len(),
            self.0.limits.user_dictionary.max_bytes,
        )?;
        let hash = ResourceHash::of(source.as_bytes());
        let key = (model.id(), hash);
        let mut state = self.0.state.lock();
        if let Some(cached) = state.users.get(&key) {
            return Ok(cached);
        }
        let dictionary = UserDictionary::compile(source, model, self.0.limits.user_dictionary)?;
        let empty_source = dictionary.is_none().then(|| Arc::from(source));
        let resolved = Arc::new(ResolvedUserDictionary {
            sha256: hash,
            model_id: model.id(),
            dictionary,
            empty_source,
        });
        state.users.insert(
            key,
            resolved.clone(),
            source.len(),
            self.0.limits.max_cached_user_dictionaries,
            self.0.limits.max_cached_user_source_bytes,
        );
        Ok(resolved)
    }
}

fn verify_hash(expected: ResourceHash, actual: ResourceHash) -> DictionaryResult<()> {
    if expected != actual {
        return Err(DictionaryError::ResourceHashMismatch { expected, actual });
    }
    Ok(())
}

struct BundledResolver;

impl DictionaryResolver for BundledResolver {
    fn resolve(&self, request: &DictionaryRequest) -> DictionaryResult<Option<DictionaryArtifact>> {
        let hash = uqa_nori_data::BUNDLE_SHA256.parse()?;
        let matches = match request {
            DictionaryRequest::Name(name) => name == DEFAULT_NORI_DICTIONARY,
            DictionaryRequest::Sha256(expected) => *expected == hash,
        };
        Ok(matches.then_some(DictionaryArtifact {
            sha256: hash,
            bytes: DictionaryBytes::Static(uqa_nori_data::BUNDLE),
        }))
    }
}

#[cfg(test)]
mod tests;
