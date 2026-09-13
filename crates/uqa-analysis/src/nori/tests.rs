//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bundle behavior exercised with an explicitly synthetic, self-contained model.

use super::dictionary::provenance::{canonical, Provenance};
use super::frame;
use super::{DictionaryError, DictionaryId, DictionaryLimits, NoriDictionary, POSTag, POSType};

mod corruption;
pub(super) mod fixtures;

#[test]
fn dictionary_preserves_ordered_words_and_optional_morphology() {
    let bytes = fixtures::bundle();
    let dictionary = NoriDictionary::from_bytes(&bytes, DictionaryLimits::default()).unwrap();
    assert_eq!(dictionary.surface_count(), 5);
    assert_eq!(dictionary.known_word_count(), 6);
    assert_eq!(dictionary.word_count(), 20);
    for (text, source, words) in [
        ("", 2, 0..1),
        ("가", 0, 1..3),
        ("가나", 4, 3..4),
        ("😀", 1, 4..5),
        ("\u{e000}", 3, 5..6),
    ] {
        let surface = dictionary.lookup(text).unwrap();
        assert_eq!(surface.source_id, source);
        assert_eq!(surface.word_ids, words);
    }
    assert!(dictionary.lookup("가난").is_none());
    assert!(dictionary.word(20).is_none());
    let units: Vec<_> = "가나다".encode_utf16().collect();
    let prefixes: Vec<_> = dictionary
        .prefixes(&units)
        .map(|(end, words)| (end, words.word_ids))
        .collect();
    assert_eq!(prefixes, [(1, 1..3), (2, 3..4)]);
    let units: Vec<_> = "😀!".encode_utf16().collect();
    assert_eq!(dictionary.prefixes(&units).next().unwrap().0, 2);
    let first = dictionary.word(0).unwrap();
    assert_eq!(first.reading(), None);
    assert!(first.morphemes().is_none());
    let compound = dictionary.word(1).unwrap();
    assert_eq!(compound.original_id(), 24);
    assert_eq!(compound.left_context(), 1);
    assert_eq!(compound.right_context(), 2);
    assert_eq!(compound.cost(), -123);
    assert_eq!(compound.pos_type(), POSType::Compound);
    assert_eq!(compound.left_pos(), POSTag::NNG);
    assert_eq!(compound.right_pos(), POSTag::NNP);
    assert_eq!(compound.reading(), Some(""));
    let morphemes: Vec<_> = compound
        .morphemes()
        .unwrap()
        .map(|m| (m.surface, m.pos))
        .collect();
    assert_eq!(morphemes, [("가", POSTag::NNG), ("나", POSTag::NNP)]);
    let inflect = dictionary.word(2).unwrap();
    assert_eq!(inflect.original_id(), 3);
    assert_eq!(inflect.pos_type(), POSType::Inflect);
    assert_eq!(inflect.morphemes().unwrap().len(), 0);
    assert_eq!(dictionary.word(3).unwrap().reading(), Some("한"));
    assert_eq!(dictionary.word(3).unwrap().pos_type(), POSType::Preanalysis);
    assert_eq!(dictionary.word(6).unwrap().original_id(), 0);
    for class in 0..14 {
        assert_eq!(
            dictionary.unknown_words(class),
            Some(u32::from(class) + 6..u32::from(class) + 7)
        );
    }
    assert!(dictionary.unknown_words(14).is_none());
}

#[test]
fn matrix_orientation_character_classes_and_pinned_unicode_are_retained() {
    let dictionary =
        NoriDictionary::from_bytes(&fixtures::bundle(), DictionaryLimits::default()).unwrap();
    assert_eq!(dictionary.connection_shape(), (3, 2));
    assert_eq!(dictionary.connection_cost(2, 0), Some(12));
    assert_eq!(dictionary.connection_cost(1, 1), Some(-21));
    assert_eq!(dictionary.connection_cost(3, 0), None);
    assert_eq!(dictionary.connection_cost(0, 2), None);
    assert_eq!(dictionary.character_class('한' as u16), 11);
    assert_eq!(dictionary.character_class('漢' as u16), 12);
    assert_eq!(dictionary.character_morphology_flags('한' as u16) & 6, 6);
    assert!(dictionary.invokes_unknown('A' as u16));
    assert!(dictionary.groups_unknown('A' as u16));
    assert!(!dictionary.invokes_unknown('한' as u16));
    let upper = dictionary.unicode('A' as u32).unwrap();
    assert_eq!(upper.category, 1);
    assert_eq!(upper.lowercase, 'a' as u32);
    assert_eq!(dictionary.unicode_script_name(upper.script), Some("LATIN"));
    assert_eq!(dictionary.unicode(0xd800).unwrap().category, 19);
    assert_eq!(
        dictionary.unicode(0x0010_ffff).unwrap().lowercase,
        0x0010_ffff
    );
    assert!(dictionary.unicode(0x0011_0000).is_none());
    assert!(dictionary.unicode_script_name(u16::MAX).is_none());
}

#[test]
fn identities_are_deterministic_serializable_and_shared() {
    let bytes = fixtures::bundle();
    assert_eq!(bytes, fixtures::bundle());
    let dictionary = NoriDictionary::from_bytes(&bytes, DictionaryLimits::default()).unwrap();
    let shared = std::sync::Arc::clone(&dictionary);
    assert!(std::sync::Arc::ptr_eq(&dictionary, &shared));
    let id = dictionary.id();
    assert_eq!(id.to_string().parse::<DictionaryId>().unwrap(), id);
    assert_eq!(
        serde_json::from_str::<DictionaryId>(&serde_json::to_string(&id).unwrap()).unwrap(),
        id
    );
    assert!("x".repeat(64).parse::<DictionaryId>().is_err());
    assert!("한".repeat(64).parse::<DictionaryId>().is_err());
    assert!(format!("{dictionary:?}").len() < 256);
    assert!(format!("{:?}", dictionary.word(1).unwrap()).len() < 512);
    let mut sections = fixtures::sections();
    sections[3].bytes[8] ^= 1;
    let changed = frame::encode(&sections, DictionaryLimits::default()).unwrap();
    assert_ne!(
        NoriDictionary::from_bytes(&changed, DictionaryLimits::default())
            .unwrap()
            .id(),
        id
    );
}

#[test]
fn every_allocation_limit_fails_before_publishing_a_dictionary() {
    let bytes = fixtures::bundle();
    let limits = DictionaryLimits::default();
    for bounded in [
        DictionaryLimits {
            max_encoded_bytes: bytes.len() - 1,
            ..limits
        },
        DictionaryLimits {
            max_decoded_bytes: 1024,
            ..limits
        },
        DictionaryLimits {
            max_manifest_bytes: 16,
            ..limits
        },
        DictionaryLimits {
            max_text_utf16: 1,
            ..limits
        },
        DictionaryLimits {
            max_strings: 0,
            ..limits
        },
    ] {
        assert!(matches!(
            NoriDictionary::from_bytes(&bytes, bounded),
            Err(DictionaryError::Limit { .. })
        ));
    }
}

#[test]
fn provenance_requires_complete_consistent_sources_and_canonical_json() {
    let json = fixtures::provenance();
    Provenance::from_value(json.clone()).unwrap();
    for pointer in [
        "/exporter_sha256",
        "/reference/lucene_commit",
        "/reference/runtime/java_vendor",
        "/reference/docker_image",
        "/reference/dictionary_source/license",
        "/files/0/sha256",
        "/reference/jars/1/artifact",
        "/reference/dictionary_resources/0/path",
    ] {
        let mut invalid = json.clone();
        *invalid.pointer_mut(pointer).unwrap() = serde_json::Value::Null;
        assert!(Provenance::from_value(invalid).is_err(), "{pointer}");
    }
    for pointer in [
        "/files",
        "/reference/jars",
        "/reference/dictionary_resources",
    ] {
        let mut invalid = json.clone();
        let entries = invalid
            .pointer_mut(pointer)
            .unwrap()
            .as_array_mut()
            .unwrap();
        entries.push(entries[0].clone());
        assert!(Provenance::from_value(invalid).is_err(), "{pointer}");
    }
    let bytes = canonical(&json).unwrap();
    Provenance::decode(&bytes, bytes.len()).unwrap();
    let mut padded = bytes;
    padded.push(b' ');
    assert!(Provenance::decode(&padded, padded.len()).is_err());
}

#[test]
fn pos_names_and_codes_round_trip_without_ordinal_confusion() {
    for ordinal in 0..POSTag::NAMES.len() {
        let tag = POSTag::from_ordinal(ordinal as u8).unwrap();
        let encoded = serde_json::to_string(&tag).unwrap();
        assert_eq!(serde_json::from_str::<POSTag>(&encoded).unwrap(), tag);
    }
    assert!(serde_json::from_str::<POSTag>("\"nng\"").is_err());
    assert!(POSTag::from_ordinal(255).is_err());
    assert!(POSType::from_ordinal(4).is_err());
}
