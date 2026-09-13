//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Versioned original-source metadata corruption and boundary coverage.

use uqa_analysis::{whitespace_analyzer, AnalyzerResources, TokenLengthPolicy};
use uqa_storage::inverted_index::{
    analyze_index_field, IndexedFieldMetadata, IndexedFieldRevision,
};

#[test]
fn field_metadata_round_trips_original_end_coordinates_and_independent_policies() {
    for policy in [
        TokenLengthPolicy::EmittedTokens,
        TokenLengthPolicy::DiscountOverlaps,
    ] {
        let analyzer = AnalyzerResources::default()
            .compile_with_length_policy(&whitespace_analyzer(), policy)
            .unwrap();
        let field = analyze_index_field(&analyzer, "한 🙂").unwrap();
        let mut metadata = IndexedFieldMetadata::new(&analyzer, &field);
        metadata.final_position_increment = u32::MAX;
        let encoded = metadata.to_bytes().unwrap();
        assert_eq!(
            (
                metadata.final_offsets.end_utf8,
                metadata.final_offsets.end_utf16
            ),
            (8, 4)
        );
        assert_eq!(
            IndexedFieldMetadata::from_bytes(&encoded).unwrap(),
            metadata
        );
        assert_eq!(
            IndexedFieldRevision::from_bytes(&metadata.revision().to_bytes().unwrap()).unwrap(),
            metadata.revision()
        );
        assert_eq!(metadata.revision(), IndexedFieldRevision::new(&analyzer));
        assert_eq!(metadata.analyzer_fingerprint.as_bytes(), &encoded[8..40]);
    }
}

#[test]
fn field_metadata_rejects_truncation_unknown_versions_reserved_bits_and_reversed_ranges() {
    let analyzer = whitespace_analyzer().compile().unwrap();
    let field = analyze_index_field(&analyzer, "value").unwrap();
    let metadata = IndexedFieldMetadata::new(&analyzer, &field);
    let encoded = metadata.to_bytes().unwrap();
    for length in 0..encoded.len() {
        assert!(IndexedFieldMetadata::from_bytes(&encoded[..length]).is_err());
    }
    let mut trailing = encoded.to_vec();
    trailing.push(0);
    assert!(IndexedFieldMetadata::from_bytes(&trailing).is_err());
    for (offset, value) in [(0, b'X'), (4, 0), (4, 2), (5, 1), (5, 3), (6, 2), (7, 1)] {
        let mut corrupted = encoded;
        corrupted[offset] = value;
        assert!(
            IndexedFieldMetadata::from_bytes(&corrupted).is_err(),
            "offset {offset} value {value}"
        );
    }
    for offset in [48, 64] {
        let mut corrupted = encoded;
        corrupted[offset..offset + 8].copy_from_slice(&u64::MAX.to_le_bytes());
        assert!(IndexedFieldMetadata::from_bytes(&corrupted).is_err());
    }
    let revision = metadata.revision().to_bytes().unwrap();
    for length in 0..revision.len() {
        assert!(IndexedFieldRevision::from_bytes(&revision[..length]).is_err());
    }
    assert!(IndexedFieldRevision::from_bytes(&encoded).is_err());
    assert!(IndexedFieldMetadata::from_bytes(&revision).is_err());
}
