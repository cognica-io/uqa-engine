//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Language-independent validation and bounded immutable resource publication.

use std::sync::Arc;

use parking_lot::Mutex;
use sha2::{Digest, Sha256};

use super::error::check_limit;
use super::limits::{DictionaryLimits, UserDictionaryLimits};
use crate::cache::Cache;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResourceCacheStats {
    pub dictionaries: usize,
    pub dictionary_encoded_bytes: usize,
    pub user_dictionaries: usize,
    pub user_source_bytes: usize,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum Error {
    #[error(transparent)]
    Dictionary(#[from] super::error::DictionaryError),
    #[error("dictionary resource is unavailable: {0}")]
    Missing(String),
    #[error("resource content differs from the requested or declared hash")]
    HashMismatch {
        expected: [u8; 32],
        actual: [u8; 32],
    },
}

#[derive(Clone, Copy)]
pub(crate) enum Request<'a> {
    Name(&'a str),
    Sha256([u8; 32]),
}

impl std::fmt::Display for Request<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Name(name) => formatter.write_str(name),
            Self::Sha256(hash) => {
                formatter.write_str("sha256:")?;
                for byte in hash {
                    write!(formatter, "{byte:02x}")?;
                }
                Ok(())
            }
        }
    }
}

pub(crate) struct Artifact {
    pub sha256: [u8; 32],
    pub bytes: DictionaryBytes,
}

struct State<D, U, I> {
    dictionaries: Cache<[u8; 32], D>,
    users: Cache<(I, [u8; 32]), U>,
}

pub(crate) struct Resources<D, U, I> {
    limits: ResourceLimits,
    state: Mutex<State<D, U, I>>,
}

impl<D, U, I: Copy + PartialEq> Resources<D, U, I> {
    pub fn new(limits: ResourceLimits) -> Self {
        Self {
            limits,
            state: Mutex::new(State {
                dictionaries: Cache::default(),
                users: Cache::default(),
            }),
        }
    }

    pub fn limits(&self) -> ResourceLimits {
        self.limits
    }

    pub fn stats(&self) -> ResourceCacheStats {
        let state = self.state.lock();
        ResourceCacheStats {
            dictionaries: state.dictionaries.len(),
            dictionary_encoded_bytes: state.dictionaries.weight(),
            user_dictionaries: state.users.len(),
            user_source_bytes: state.users.weight(),
        }
    }

    pub fn load<E: From<Error>>(
        &self,
        request: Request<'_>,
        resolve: impl FnOnce() -> Result<Option<Artifact>, E>,
        decode: impl FnOnce(Artifact) -> Result<D, E>,
    ) -> Result<Arc<D>, E> {
        if let Request::Sha256(hash) = request {
            if let Some(cached) = self.state.lock().dictionaries.get(&hash) {
                return Ok(cached);
            }
        }
        // Host callbacks may reenter resource resolution, so they run outside the cache lock.
        let artifact = resolve()?.ok_or_else(|| Error::Missing(request.to_string()))?;
        check_limit(
            "encoded bytes",
            artifact.bytes.as_ref().len(),
            self.limits.dictionary.max_encoded_bytes,
        )
        .map_err(Error::from)?;
        let hash: [u8; 32] = Sha256::digest(artifact.bytes.as_ref()).into();
        verify_hash(artifact.sha256, hash)?;
        if let Request::Sha256(expected) = request {
            verify_hash(expected, hash)?;
        }
        let mut state = self.state.lock();
        if let Some(cached) = state.dictionaries.get(&hash) {
            return Ok(cached);
        }
        // Serialize validation/publication so simultaneous misses share one decoded allocation.
        let weight = artifact.bytes.as_ref().len();
        let resolved = Arc::new(decode(artifact)?);
        state.dictionaries.insert(
            hash,
            resolved.clone(),
            weight,
            self.limits.max_cached_dictionaries,
            self.limits.max_cached_encoded_bytes,
        );
        Ok(resolved)
    }

    pub fn compile_user<E: From<Error>>(
        &self,
        source: &str,
        model_id: I,
        compile: impl FnOnce([u8; 32]) -> Result<U, E>,
    ) -> Result<Arc<U>, E> {
        check_limit(
            "user dictionary bytes",
            source.len(),
            self.limits.user_dictionary.max_bytes,
        )
        .map_err(Error::from)?;
        let hash = Sha256::digest(source.as_bytes()).into();
        let key = (model_id, hash);
        let mut state = self.state.lock();
        if let Some(cached) = state.users.get(&key) {
            return Ok(cached);
        }
        let resolved = Arc::new(compile(hash)?);
        state.users.insert(
            key,
            resolved.clone(),
            source.len(),
            self.limits.max_cached_user_dictionaries,
            self.limits.max_cached_user_source_bytes,
        );
        Ok(resolved)
    }
}

fn verify_hash(expected: [u8; 32], actual: [u8; 32]) -> Result<(), Error> {
    if expected != actual {
        return Err(Error::HashMismatch { expected, actual });
    }
    Ok(())
}
