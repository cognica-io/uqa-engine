//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{DictionaryError, DictionaryLimits, KuromojiDictionary};

pub(super) mod analysis;
pub(super) mod fixtures;

#[test]
fn dictionary_preserves_japanese_attributes_and_surface_identity() {
    let model =
        KuromojiDictionary::from_bytes(&fixtures::bundle(), DictionaryLimits::default()).unwrap();
    let reign = model.lookup("令和").unwrap();
    assert_eq!(reign.source_id, 1);
    assert_eq!(reign.word_ids, 0..1);
    let word = model.word(0).unwrap();
    assert_eq!(word.part_of_speech(), "名詞-一般");
    assert_eq!(word.base_form(), None);
    assert_eq!(word.reading(), Some("レイワ"));
    assert_eq!(word.pronunciation(), Some(""));
    assert_eq!(word.inflection_type(), None);
    let word = model.word(1).unwrap();
    assert_eq!(word.part_of_speech(), "動詞-自立");
    assert_eq!(word.base_form(), Some("食べる"));
    assert_eq!(word.reading(), Some("タベタ"));
    assert_eq!(word.pronunciation(), Some("タベタ"));
    assert_eq!(word.inflection_type(), Some("一段"));
    assert_eq!(word.inflection_form(), Some("連用タ接続"));
    assert_eq!(word.original_id(), 7);
    assert_eq!(word.cost(), -123);
    assert_eq!((word.left_context(), word.right_context()), (0, 0));
    assert_eq!(model.lookup("missing"), None);
    assert!(model.word(14).is_none());
    let input: Vec<_> = "食べた後".encode_utf16().collect();
    assert_eq!(
        model.prefixes(&input).collect::<Vec<_>>(),
        [(3, model.lookup("食べた").unwrap())]
    );
    assert_eq!(model.unknown_words(0), Some(2..3));
    assert_eq!(model.unknown_words(12), None);
    assert_eq!(model.connection_cost(0, 0), Some(0));
    assert_eq!(model.connection_cost(1, 0), None);
    assert_eq!(model.default_stop_words(), ["は"]);
    assert_eq!(model.default_stop_tags(), ["助詞"]);
    assert_eq!(model.completion_mappings()[0].key(), "レ");
    assert_eq!(model.completion_mappings()[0].alternatives(), ["re"]);
    assert!(!model.is_kanji('A' as u16));
    assert!(model.groups_unknown('A' as u16));
    assert!(!model.invokes_unknown('A' as u16));
    assert_eq!(model.unicode(0xd800).unwrap().category, 19);
    assert!(model.unicode(0x11_0000).is_none());
    assert_eq!(model.unicode_script_name(2), Some("UNKNOWN"));
    let id = model.id().to_string();
    assert_eq!(id.parse::<super::DictionaryId>().unwrap(), model.id());
    assert_eq!(
        serde_json::from_str::<super::DictionaryId>(&format!("\"{id}\"")).unwrap(),
        model.id()
    );
}

#[test]
fn every_truncated_frame_and_trailing_byte_is_rejected() {
    let mut bytes = fixtures::bundle();
    for end in 0..bytes.len() {
        assert!(
            KuromojiDictionary::from_bytes(&bytes[..end], DictionaryLimits::default()).is_err(),
            "accepted {end}"
        );
    }
    bytes.push(0);
    assert!(KuromojiDictionary::from_bytes(&bytes, DictionaryLimits::default()).is_err());
}

#[test]
fn schema_and_checksums_are_validated_before_model_publication() {
    let bytes = fixtures::bundle();
    let mut changed = bytes.clone();
    changed[..8].copy_from_slice(b"UQANORI\0");
    assert!(matches!(
        KuromojiDictionary::from_bytes(&changed, DictionaryLimits::default()),
        Err(DictionaryError::Invalid { .. })
    ));
    changed = bytes.clone();
    changed[8..12].copy_from_slice(&2_u32.to_le_bytes());
    assert!(matches!(
        KuromojiDictionary::from_bytes(&changed, DictionaryLimits::default()),
        Err(DictionaryError::Version(2))
    ));
    changed = bytes.clone();
    changed[24] ^= 1;
    assert!(matches!(
        KuromojiDictionary::from_bytes(&changed, DictionaryLimits::default()),
        Err(DictionaryError::Checksum(0))
    ));
    changed = bytes;
    *changed.last_mut().unwrap() ^= 1;
    assert!(KuromojiDictionary::from_bytes(&changed, DictionaryLimits::default()).is_err());
}

#[test]
fn rehashed_invalid_words_and_character_semantics_are_rejected() {
    let mutations: &[(usize, usize, &[u8])] = &[
        (1, 20, &u32::MAX.to_le_bytes()), // Required POS becomes absent.
        (1, 12, &1_u16.to_le_bytes()),    // Left context is outside the one-row matrix.
        (1, 18, &1_u16.to_le_bytes()),    // Reserved word bytes must remain zero.
        (4, 116, &[10, 0]),               // Kanji class without its Japanese attribute.
        (4, 4, &[4]),                     // Unknown invoke/group flag bits.
        (5, 16, &[17]),                   // Invalid Java character category.
    ];
    for &(section, offset, replacement) in mutations {
        let mut sections = fixtures::sections();
        sections[section].bytes[offset..offset + replacement.len()].copy_from_slice(replacement);
        let bytes = super::frame::encode(&sections, DictionaryLimits::default()).unwrap();
        assert!(
            KuromojiDictionary::from_bytes(&bytes, DictionaryLimits::default()).is_err(),
            "accepted section {section} offset {offset}"
        );
    }
}

#[test]
fn completion_alternatives_and_provenance_counts_are_not_silently_repaired() {
    let mut sections = fixtures::sections();
    let mut output = crate::morphology::io::Writer::default();
    output.u32(0).unwrap();
    output.u32(0).unwrap();
    output.u32(1).unwrap();
    output.text("レ").unwrap();
    output.u32(0).unwrap();
    sections[6].bytes = output.0;
    let bytes = super::frame::encode(&sections, DictionaryLimits::default()).unwrap();
    assert!(matches!(
        KuromojiDictionary::from_bytes(&bytes, DictionaryLimits::default()),
        Err(DictionaryError::Invalid {
            reason: "completion mapping has no nonempty alternatives",
            ..
        })
    ));
    let mut sections = fixtures::sections();
    let mut manifest = fixtures::provenance();
    manifest["model"]["word_count"] = 3.into();
    sections[7].bytes = super::provenance::canonical(&manifest).unwrap();
    let bytes = super::frame::encode(&sections, DictionaryLimits::default()).unwrap();
    assert!(matches!(
        KuromojiDictionary::from_bytes(&bytes, DictionaryLimits::default()),
        Err(DictionaryError::Invalid {
            reason: "model counts differ from decoded tables",
            ..
        })
    ));
}

#[test]
fn all_loader_limits_fail_without_invalidating_retained_models() {
    let bytes = fixtures::bundle();
    let retained = KuromojiDictionary::from_bytes(&bytes, DictionaryLimits::default()).unwrap();
    let limits = [
        DictionaryLimits {
            max_encoded_bytes: 1,
            ..DictionaryLimits::default()
        },
        DictionaryLimits {
            max_decoded_bytes: 1,
            ..DictionaryLimits::default()
        },
        DictionaryLimits {
            max_manifest_bytes: 1,
            ..DictionaryLimits::default()
        },
        DictionaryLimits {
            max_text_utf16: 1,
            ..DictionaryLimits::default()
        },
        DictionaryLimits {
            max_strings: 11,
            ..DictionaryLimits::default()
        },
    ];
    for limit in limits {
        assert!(matches!(
            KuromojiDictionary::from_bytes(&bytes, limit),
            Err(DictionaryError::Limit { .. })
        ));
        assert_eq!(retained.word(1).unwrap().base_form(), Some("食べる"));
    }
    let model = KuromojiDictionary::from_bytes(
        &bytes,
        DictionaryLimits {
            max_strings: 12,
            ..DictionaryLimits::default()
        },
    )
    .unwrap();
    assert_eq!(model.id(), retained.id());
}

#[test]
fn stop_resources_require_nonempty_unique_utf16_ordered_entries() {
    for words in [["a", "a"], ["b", "a"], ["", "a"], ["\u{e000}", "😀"]] {
        let mut sections = fixtures::sections();
        let mut output = crate::morphology::io::Writer::default();
        super::analysis::AnalysisData {
            stop_words: words.map(str::to_owned).into(),
            stop_tags: vec![],
            completion: vec![],
            completion_lexicon: crate::morphology::lexicon::Builder::new()
                .finish(0)
                .unwrap(),
        }
        .encode(&mut output)
        .unwrap();
        sections[6].bytes = output.0;
        sections[6].records = 0;
        let mut manifest = fixtures::provenance();
        manifest["model"]["stop_word_count"] = 2.into();
        manifest["model"]["stop_tag_count"] = 0.into();
        manifest["model"]["completion_mapping_count"] = 0.into();
        sections[7].bytes = super::provenance::canonical(&manifest).unwrap();
        let bytes = super::frame::encode(&sections, DictionaryLimits::default()).unwrap();
        assert!(matches!(
            KuromojiDictionary::from_bytes(&bytes, DictionaryLimits::default()),
            Err(DictionaryError::Invalid {
                reason: "empty, duplicate or unordered key",
                ..
            })
        ));
    }
}

#[test]
fn embedded_dictionary_matches_its_model_and_japanese_resources() {
    let model =
        KuromojiDictionary::from_bytes(uqa_kuromoji_data::BUNDLE, DictionaryLimits::default())
            .unwrap();
    assert_eq!(model.id().to_string(), uqa_kuromoji_data::DICTIONARY_ID);
    assert_eq!(model.surface_count(), 325_872);
    assert_eq!(model.known_word_count(), 392_127);
    assert_eq!(model.word_count(), 392_168);
    assert_eq!(model.connection_shape(), (1316, 1316));
    assert_eq!(model.default_stop_words().len(), 109);
    assert_eq!(model.default_stop_tags().len(), 27);
    assert_eq!(model.completion_mappings().len(), 329);
    let reign = model.lookup("令和").unwrap();
    assert!(reign
        .word_ids
        .into_iter()
        .any(|id| model.word(id).unwrap().reading() == Some("レイワ")));
    assert!(model.is_kanji('令' as u16));
    assert!(!model.is_kanji('あ' as u16));
    #[cfg(feature = "kuromoji-tools")]
    super::pack::verify_dictionary(&model, DictionaryLimits::default()).unwrap();
}
