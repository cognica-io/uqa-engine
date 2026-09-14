//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Packaged immutable bytes must retain their declared identity and provenance.

use sha2::{Digest, Sha256};
use uqa_nori_data::{BUNDLE, BUNDLE_SHA256, DICTIONARY_ID, MODEL_MANIFEST, RESOURCE_MANIFEST};

#[test]
fn embedded_bytes_match_the_packaged_identity_and_manifests() {
    assert_eq!(BUNDLE.len(), 9_829_534);
    assert_eq!(format!("{:x}", Sha256::digest(BUNDLE)), BUNDLE_SHA256);
    let model: serde_json::Value = serde_json::from_str(MODEL_MANIFEST).unwrap();
    assert_eq!(model["model"]["surface_count"], 774_582);
    let resource: serde_json::Value = serde_json::from_str(RESOURCE_MANIFEST).unwrap();
    assert_eq!(resource["dictionary_id"], DICTIONARY_ID);
    assert_eq!(resource["files"][0]["sha256"], BUNDLE_SHA256);
}
