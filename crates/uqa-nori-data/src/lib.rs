//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Pinned dictionary bytes and provenance, with no build scripts, downloads, or runtime dependencies.

#![no_std]

/// Portable Nori dictionary. Validate with the matching UQA analysis loader before use.
pub static BUNDLE: &[u8] = include_bytes!("../data/nori.uqan");

/// Semantic dictionary identity, independent of transport compression.
pub const DICTIONARY_ID: &str = "ee85ee5796c706ea7288e44e64c7f58ba7bd22cea5683c1f4196c29a2435d74a";

/// SHA-256 of the exact packaged bundle bytes.
pub const BUNDLE_SHA256: &str = "0d920523991ed60909972d85df630c65747ff5d49e1138e4998ca39f0079538d";

/// Full reference, vocabulary, and neutral-model export provenance.
pub const MODEL_MANIFEST: &str = include_str!("../data/model_manifest.json");

/// Bundle and attribution-file hashes used when checking source and package inventories.
pub const RESOURCE_MANIFEST: &str = include_str!("../data/resource_manifest.json");
