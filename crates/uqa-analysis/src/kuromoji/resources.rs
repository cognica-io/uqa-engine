//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Explicit immutable dictionary resolution and bounded content-based interning.

use std::sync::{Arc, OnceLock};

use super::{DictionaryId, DictionaryResult, KuromojiDictionary, UserDictionary};

mod hash;
pub(crate) mod preparation;

use crate::morphology::resources::{handles, Artifact, Resources};
pub use crate::morphology::resources::{DictionaryBytes, ResourceCacheStats, ResourceLimits};
pub use hash::ResourceHash;

pub const DEFAULT_KUROMOJI_DICTIONARY: &str = "lucene-10.5.1";

handles::dictionary_resolver!(ResourceHash, super::DictionaryError);

pub type ResolvedDictionary = handles::ResolvedDictionary<KuromojiDictionary, ResourceHash>;
pub type ResolvedUserDictionary =
    handles::ResolvedUserDictionary<UserDictionary, DictionaryId, ResourceHash>;

impl ResolvedUserDictionary {
    pub fn source(&self) -> &str {
        self.rule_source(UserDictionary::source)
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
        self.0.resources.load(
            request.as_request(),
            || {
                self.0
                    .resolver
                    .resolve(request)
                    .map(|artifact| artifact.map(|a| a.map_hash(ResourceHash::into_bytes)))
            },
            |artifact| {
                crate::morphology::work::inline(ResolvedDictionary::from_artifact_with(
                    artifact,
                    self.limits().dictionary,
                    &mut (),
                ))
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
            Ok(ResolvedUserDictionary::new(
                ResourceHash::from_bytes(hash),
                model.id(),
                dictionary,
                source,
            ))
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
