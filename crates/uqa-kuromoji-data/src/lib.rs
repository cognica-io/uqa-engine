//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Pinned dictionary bytes and provenance, with no build scripts, downloads, or runtime dependencies.

#![no_std]

/// Portable Kuromoji dictionary. Validate with the matching UQA analysis loader before use.
pub static BUNDLE: &[u8] = include_bytes!("../data/kuromoji.uqak");

/// Semantic dictionary identity, independent of transport compression.
pub const DICTIONARY_ID: &str = "dd2691ffee7f5a0d2c29c1a6e66dee249116767208a8cd8e783a6aa479100c8f";

/// SHA-256 of the exact packaged bundle bytes.
pub const BUNDLE_SHA256: &str = "bd3dd53f609006e72d0aa6e94ec6067400ac2be328ca0c39263ab36c3197aa97";

/// Full reference, vocabulary, and neutral-model export provenance.
pub const MODEL_MANIFEST: &str = include_str!("../data/model_manifest.json");

/// Bundle and attribution-file hashes used when checking source and package inventories.
pub const RESOURCE_MANIFEST: &str = include_str!("../data/resource_manifest.json");
