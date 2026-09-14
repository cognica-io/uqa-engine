//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The packaged model must reconstruct every reviewed neutral value without external files.

use sha2::{Digest, Sha256};
use uqa_analysis::nori::{pack, DictionaryLimits};
use uqa_nori_data::{BUNDLE, BUNDLE_SHA256, DICTIONARY_ID, MODEL_MANIFEST, RESOURCE_MANIFEST};

#[test]
fn embedded_bundle_matches_the_full_reference_and_packaged_provenance() {
    assert_eq!(BUNDLE.len(), 9_829_534);
    assert_eq!(format!("{:x}", Sha256::digest(BUNDLE)), BUNDLE_SHA256);
    let dictionary = super::nori_resources::model();
    assert_eq!(dictionary.id().to_string(), DICTIONARY_ID);
    assert_eq!(dictionary.surface_count(), 774_582);
    assert_eq!(dictionary.known_word_count(), 816_283);
    assert_eq!(dictionary.word_count(), 816_297);
    assert_eq!(dictionary.connection_shape(), (3822, 2693));
    assert!(dictionary.lookup("한국").is_some());
    assert_eq!(
        dictionary.unicode('İ' as u32).unwrap().lowercase,
        'i' as u32
    );
    assert_eq!(
        dictionary.unicode('Σ' as u32).unwrap().lowercase,
        'σ' as u32
    );
    let manifest: serde_json::Value = serde_json::from_str(MODEL_MANIFEST).unwrap();
    assert_eq!(dictionary.provenance(), &manifest);
    let resource: serde_json::Value = serde_json::from_str(RESOURCE_MANIFEST).unwrap();
    assert_eq!(resource["dictionary_id"], DICTIONARY_ID);
    assert_eq!(resource["files"][0]["sha256"], BUNDLE_SHA256);
    pack::verify_dictionary(dictionary, DictionaryLimits::default()).unwrap();
}
