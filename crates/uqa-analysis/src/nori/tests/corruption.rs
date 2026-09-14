//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Malformed transport and checksum-correct invalid tables must never be published.

use sha2::{Digest, Sha256};

use super::fixtures;
use crate::nori::dictionary::provenance::canonical;
use crate::nori::frame::{self, Section};
use crate::nori::{DictionaryError, DictionaryLimits, NoriDictionary};

fn rejected(sections: &[Section]) {
    let bytes = frame::encode(sections, DictionaryLimits::default()).unwrap();
    let error = NoriDictionary::from_bytes(&bytes, DictionaryLimits::default()).unwrap_err();
    assert!(!matches!(error, DictionaryError::Checksum(_)), "{error}");
}

fn set_u32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

#[test]
fn malformed_word_contexts_metadata_and_strings_are_rejected_after_hash_validation() {
    for (offset, value) in [
        (8, u32::MAX),
        (12, 0xffff),
        (24, 100),
        (28, 100),
        (32, 1),
        (36, 1),
    ] {
        let mut sections = fixtures::sections();
        set_u32(&mut sections[1].bytes, offset, value);
        rejected(&sections);
    }
    for offset in [18, 19, 20, 21] {
        let mut sections = fixtures::sections();
        sections[1].bytes[offset] = 255;
        rejected(&sections);
    }
    let mut sections = fixtures::sections();
    // The first nonempty pooled string begins after count, empty length, and its own length.
    sections[2].bytes[12] = 255;
    rejected(&sections);
    let mut sections = fixtures::sections();
    let last = sections[2].bytes.len() - 5;
    set_u32(&mut sections[2].bytes, last, 999);
    rejected(&sections);
}

#[test]
fn cyclic_lexicons_duplicate_sources_and_incomplete_word_coverage_are_rejected() {
    let sections = fixtures::sections();
    let nodes = u32::from_le_bytes(sections[0].bytes[4..8].try_into().unwrap()) as usize;
    let arcs = u32::from_le_bytes(sections[0].bytes[8..12].try_into().unwrap()) as usize;
    let arc_start = 12 + nodes * 5;
    let surface_start = arc_start + arcs * 6 + 4;
    let mut invalid = fixtures::sections();
    set_u32(&mut invalid[0].bytes, arc_start + 2, nodes as u32);
    rejected(&invalid);
    let mut invalid = fixtures::sections();
    invalid[0].bytes[surface_start + 2] = 0;
    rejected(&invalid);
    let mut invalid = fixtures::sections();
    invalid[0].bytes[surface_start + 1] = 0;
    rejected(&invalid);
    let mut invalid = fixtures::sections();
    // Root is the last state, and its last outgoing label must sort after its previous label.
    let last_arc = arc_start + (arcs - 1) * 6;
    invalid[0].bytes[last_arc..last_arc + 2].copy_from_slice(&0_u16.to_le_bytes());
    rejected(&invalid);
}

#[test]
fn wrong_dimensions_character_flags_and_unicode_intervals_are_rejected() {
    let mut sections = fixtures::sections();
    set_u32(&mut sections[3].bytes, 0, 0);
    rejected(&sections);
    let mut sections = fixtures::sections();
    sections[4].bytes[4] = 4;
    rejected(&sections);
    let mut sections = fixtures::sections();
    set_u32(&mut sections[4].bytes, 18, 5);
    rejected(&sections);
    let mut sections = fixtures::sections();
    sections[4].bytes[134] = 14;
    rejected(&sections);
    let mut sections = fixtures::sections();
    sections[4].bytes[135] ^= 4;
    rejected(&sections);
    for (offset, value) in [(0, 0), (8, 0), (16, u32::MAX), (16, 0xd800)] {
        let mut sections = fixtures::sections();
        set_u32(&mut sections[5].bytes, offset, value);
        rejected(&sections);
    }
    for (offset, value) in [(12, 17), (13, 8), (14, 255), (48, 0)] {
        let mut sections = fixtures::sections();
        sections[5].bytes[offset] = value;
        rejected(&sections);
    }
}

#[test]
fn directory_counts_and_manifest_counts_must_match_decoded_tables() {
    for index in 0..7 {
        let mut sections = fixtures::sections();
        sections[index].records += 1;
        rejected(&sections);
    }
    let mut sections = fixtures::sections();
    let mut json = fixtures::provenance();
    json["model"]["word_count"] = 7.into();
    sections[6].bytes = canonical(&json).unwrap();
    rejected(&sections);
}

#[test]
fn truncation_mutations_unknown_versions_and_trailing_data_fail_without_panics() {
    let bytes = fixtures::bundle();
    let limits = DictionaryLimits::default();
    for cut in [
        0,
        7,
        8,
        12,
        16,
        24,
        55,
        56,
        71,
        127,
        559,
        560,
        bytes.len() - 1,
    ] {
        assert!(
            NoriDictionary::from_bytes(&bytes[..cut], limits).is_err(),
            "{cut}"
        );
    }
    for index in (0..bytes.len()).step_by(31) {
        let mut invalid = bytes.clone();
        invalid[index] ^= 0xff;
        assert!(
            NoriDictionary::from_bytes(&invalid, limits).is_err(),
            "{index}"
        );
    }
    let mut invalid = bytes.clone();
    set_u32(&mut invalid, 8, 99);
    assert!(matches!(
        NoriDictionary::from_bytes(&invalid, limits),
        Err(DictionaryError::Version(99))
    ));
    let mut invalid = bytes;
    invalid.push(0);
    assert!(NoriDictionary::from_bytes(&invalid, limits).is_err());
}

// This independent frame writer tests identity separately from the production encoder's codec choice.
fn transport(
    sections: &[Section],
    mut encode: impl FnMut(u32, &[u8]) -> (u32, Vec<u8>),
) -> Vec<u8> {
    let mut identity = Sha256::new();
    identity.update(b"UQA Nori semantic dictionary\0");
    identity.update(1_u32.to_le_bytes());
    for section in sections {
        identity.update(section.kind.to_le_bytes());
        identity.update((section.bytes.len() as u64).to_le_bytes());
        identity.update(section.records.to_le_bytes());
        identity.update(Sha256::digest(&section.bytes));
    }
    let mut out = b"UQANORI\0".to_vec();
    out.extend(1_u32.to_le_bytes());
    out.extend(7_u32.to_le_bytes());
    out.extend(
        sections
            .iter()
            .map(|s| s.bytes.len() as u64)
            .sum::<u64>()
            .to_le_bytes(),
    );
    out.extend(identity.finalize());
    let mut payloads = Vec::new();
    let mut offset = 560_u64;
    for section in sections {
        let (codec, bytes) = encode(section.kind, &section.bytes);
        out.extend(section.kind.to_le_bytes());
        out.extend(codec.to_le_bytes());
        out.extend(offset.to_le_bytes());
        out.extend((bytes.len() as u64).to_le_bytes());
        out.extend((section.bytes.len() as u64).to_le_bytes());
        out.extend(section.records.to_le_bytes());
        out.extend(Sha256::digest(&section.bytes));
        offset += bytes.len() as u64;
        payloads.push(bytes);
    }
    for payload in payloads {
        out.extend(payload);
    }
    out
}

#[test]
fn identity_is_transport_independent_and_zlib_consumes_exactly_one_stream() {
    let sections = fixtures::sections();
    let limits = DictionaryLimits::default();
    let raw = transport(&sections, |_, bytes| (0, bytes.to_vec()));
    let plain = NoriDictionary::from_bytes(&raw, limits).unwrap();
    let compressed = NoriDictionary::from_bytes(&fixtures::bundle(), limits).unwrap();
    assert_eq!(plain.id(), compressed.id());
    for fault in 0..3 {
        let bad = transport(&sections, |kind, bytes| {
            if kind != 1 {
                return (0, bytes.to_vec());
            }
            let mut bytes = miniz_oxide::deflate::compress_to_vec_zlib(bytes, 9);
            match fault {
                0 => bytes.push(0),
                1 => {
                    bytes.pop();
                }
                _ => bytes
                    .extend_from_slice(&miniz_oxide::deflate::compress_to_vec_zlib(b"extra", 9)),
            }
            (1, bytes)
        });
        let error = NoriDictionary::from_bytes(&bad, limits).unwrap_err();
        assert!(matches!(
            error,
            DictionaryError::Invalid {
                section: "compressed section",
                ..
            }
        ));
    }
}
