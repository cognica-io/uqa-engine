//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn simple() -> OccurrencePosting {
    OccurrencePosting {
        doc_id: 1,
        doc_length: 1,
        occurrences: vec![edge(0, 1, None)],
    }
}

#[test]
fn graph_wire_format_has_fixed_bytes_and_rejects_truncation_and_unknown_fields() {
    let (scores, positions) = encode_occurrence_cluster(&[simple()]).unwrap();
    assert_eq!(
        positions,
        b"UQCP\x02\0\0\0\x01\0\0\0\x02\0\0\0\0\0\0\0\x03\0\0\0\0\x01\0"
    );
    assert_eq!(scores, b"UQCS\x02\0\0\0\x01\0\0\0\x01\0\0\0\x01\0\x01\0\x2c\0\0\0\x2d\0\0\0\x2d\0\0\0\x2e\0\0\0\x2e\0\0\0\x2f\0\0\0\x01\x01\x01");
    for length in 0..scores.len() {
        assert!(decode_occurrence_cluster(0, &scores[..length], &positions).is_err());
    }
    for length in 0..positions.len() {
        assert!(decode_occurrence_cluster(0, &scores, &positions[..length]).is_err());
    }
    for offset in [0, 4, 5, 8, 12, 16, 20, 25, 26] {
        let mut changed = positions.clone();
        changed[offset] = 0xff;
        assert!(
            decode_occurrence_cluster(0, &scores, &changed).is_err(),
            "offset {offset}"
        );
    }
    let mut extra = positions.clone();
    extra.push(0);
    assert!(decode_occurrence_cluster(0, &scores, &extra).is_err());
    let mut extra = scores.clone();
    extra.push(0);
    assert!(decode_occurrence_cluster(0, &extra, &positions).is_err());
}

#[test]
fn corrupt_occurrences_cannot_wrap_positions_or_offsets_or_bypass_canonical_varints() {
    let (scores, positions) = encode_occurrence_cluster(&[simple()]).unwrap();
    let with_payload = |payload: &[u8]| {
        let mut changed = positions[..20].to_vec();
        changed.extend_from_slice(&u32::try_from(payload.len()).unwrap().to_le_bytes());
        changed.extend_from_slice(payload);
        changed
    };
    for payload in [
        vec![0, 0, 0],
        vec![0, 1, 2],
        vec![0x80, 0, 1, 0],
        vec![0xff, 0xff, 0xff, 0xff, 0x0f, 1, 0],
        vec![0, 0x80, 0x80, 0x80, 0x80, 0x10, 0],
        vec![0, 1, 0, 0],
        vec![
            0, 1, 1, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 1, 1, 0, 0,
        ],
    ] {
        assert!(decode_occurrence_cluster(0, &scores, &with_payload(&payload)).is_err());
    }
    for occurrence in [
        edge(0, 0, None),
        edge(u32::MAX, 1, None),
        edge(0, 1, Some(span(2, 1, 0, 1))),
        edge(0, 1, Some(span(0, 1, 2, 1))),
    ] {
        let mut entry = simple();
        entry.occurrences = vec![occurrence];
        assert!(encode_occurrence_cluster(&[entry]).is_err());
    }
    let mut empty = simple();
    empty.occurrences.clear();
    assert!(encode_occurrence_cluster(&[empty]).is_err());
    let mut zero_length = simple();
    zero_length.doc_length = 0;
    assert!(encode_occurrence_cluster(&[zero_length]).is_err());
    let mut reversed = simple();
    reversed.occurrences = vec![edge(2, 1, None), edge(1, 1, None)];
    assert!(encode_occurrence_cluster(&[reversed]).is_err());
    assert!(encode_occurrence_cluster(&[simple(), simple()]).is_err());
    let mut outside = simple();
    outside.doc_id = 65_536;
    assert!(encode_occurrence_cluster(&[simple(), outside]).is_err());
}

#[test]
fn accepted_byte_mutations_reencode_canonically_without_panics() {
    let entry = OccurrencePosting {
        doc_id: 123,
        doc_length: 1,
        occurrences: vec![
            edge(0, 2, Some(span(2, 6, 3, 4))),
            edge(0, 1, None),
            edge(2, 1, Some(span(9, 12, 7, 8))),
        ],
    };
    let (scores, positions) = encode_occurrence_cluster(&[entry]).unwrap();
    for side in [false, true] {
        let original = if side { &scores } else { &positions };
        for offset in 0..original.len() {
            for bit in 0..8 {
                let mut changed = original.clone();
                changed[offset] ^= 1 << bit;
                let (scores, positions) = if side {
                    (&changed, &positions)
                } else {
                    (&scores, &changed)
                };
                if let Ok(decoded) = decode_occurrence_cluster(0, scores, positions) {
                    let (encoded_scores, encoded_positions) =
                        encode_occurrence_cluster(&decoded).unwrap();
                    assert_eq!(&encoded_scores, scores);
                    assert_eq!(&encoded_positions, positions);
                }
            }
        }
    }
}

#[test]
fn binary_vocabulary_rejects_truncation_duplicates_and_noncanonical_units() {
    let keys = [
        TokenTermKey::from_text(""),
        TokenTermKey::from_text("a"),
        TokenTermKey::from_term(&TokenTerm::from_utf16(vec![0xd800])),
    ];
    let bytes = encode_term_keys(&keys).unwrap();
    for length in 0..bytes.len() {
        assert!(decode_term_keys(&bytes[..length]).is_err());
    }
    let mut extra = bytes.clone();
    extra.push(0);
    assert!(decode_term_keys(&extra).is_err());
    assert!(encode_term_keys(&[keys[0].clone(), keys[0].clone()]).is_err());
    assert!(encode_term_keys(&[keys[1].clone(), keys[0].clone()]).is_err());
    let mut noncanonical = bytes;
    let n = noncanonical.len();
    noncanonical[n - 2..].copy_from_slice(&[0, 97]);
    assert!(decode_term_keys(&noncanonical).is_err());
}
