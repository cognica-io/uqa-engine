//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

const FORMAT: Format = Format {
    magic: b"UQATEST\0",
    version: 1,
    section_count: 1,
    identity_domain: b"independent dictionary frame fixture\0",
};

fn wire(codec: u32, stored: &[u8], decoded: usize, hash: [u8; 32]) -> Vec<u8> {
    let mut identity = Sha256::new();
    identity.update(FORMAT.identity_domain);
    identity.update(1_u32.to_le_bytes());
    identity.update(1_u32.to_le_bytes());
    identity.update((decoded as u64).to_le_bytes());
    identity.update(17_u64.to_le_bytes());
    identity.update(hash);
    let mut wire = Vec::new();
    wire.extend_from_slice(FORMAT.magic);
    wire.extend_from_slice(&1_u32.to_le_bytes());
    wire.extend_from_slice(&1_u32.to_le_bytes());
    wire.extend_from_slice(&(decoded as u64).to_le_bytes());
    wire.extend_from_slice(&identity.finalize());
    wire.extend_from_slice(&1_u32.to_le_bytes());
    wire.extend_from_slice(&codec.to_le_bytes());
    wire.extend_from_slice(&128_u64.to_le_bytes());
    wire.extend_from_slice(&(stored.len() as u64).to_le_bytes());
    wire.extend_from_slice(&(decoded as u64).to_le_bytes());
    wire.extend_from_slice(&17_u64.to_le_bytes());
    wire.extend_from_slice(&hash);
    wire.extend_from_slice(stored);
    wire
}

fn decoded(input: &FrameInput<'_>) -> usize {
    input.sections.iter().map(|s| s.bytes.len()).sum::<usize>()
        + input
            .current
            .as_ref()
            .map_or(0, |current| current.data.len())
}

fn consumed(input: &FrameInput<'_>, stored: usize) -> usize {
    input.current.as_ref().map_or_else(
        || if input.sections.is_empty() { 0 } else { stored },
        |current| current.offset,
    )
}

#[test]
fn dictionary_frame_fragments_preserve_exact_plain_and_compressed_payloads() {
    for payload in [
        Vec::new(),
        b"native dictionary".to_vec(),
        vec![42; 1 << 20],
        (0..180_000)
            .map(|n| ((n * 73 + n / 19) % 251) as u8)
            .collect(),
    ] {
        for codec in [0, 1] {
            let stored = if codec == 0 {
                payload.clone()
            } else {
                miniz_oxide::deflate::compress_to_vec_zlib(&payload, 9)
            };
            let wire = wire(
                codec,
                &stored,
                payload.len(),
                Sha256::digest(&payload).into(),
            );
            let mut input = FrameInput::new(&FORMAT, &wire, DictionaryLimits::default()).unwrap();
            let mut calls = 0;
            let (id, sections) = loop {
                let before = (consumed(&input, stored.len()), decoded(&input));
                calls += 1;
                assert!(calls < 4096);
                match input.advance().unwrap() {
                    ControlFlow::Continue(next) => {
                        assert!(consumed(&next, stored.len()) - before.0 <= FRAGMENT);
                        assert!(decoded(&next) - before.1 <= FRAGMENT);
                        input = next;
                    }
                    ControlFlow::Break((id, sections)) => {
                        assert!(stored.len() - before.0 <= FRAGMENT);
                        assert!(payload.len() - before.1 <= FRAGMENT);
                        break (id, sections);
                    }
                }
            };
            assert_eq!(id.as_slice(), &wire[24..56]);
            assert!(calls >= payload.len().div_ceil(FRAGMENT));
            assert_eq!(sections.len(), 1);
            assert_eq!(sections[0].kind, 1);
            assert_eq!(sections[0].records, 17);
            assert_eq!(sections[0].bytes, payload);
            assert_eq!(sections[0].bytes.capacity(), payload.len());
            let ordinary =
                super::super::decode(&FORMAT, &wire, DictionaryLimits::default()).unwrap();
            assert_eq!(ordinary.0, id);
            assert_eq!(ordinary.1[0].bytes, payload);
        }
    }
}

#[test]
fn dictionary_frame_rejects_incomplete_streams_excess_output_and_wrong_checksums() {
    let payload = vec![42; 50_000];
    let stored = miniz_oxide::deflate::compress_to_vec_zlib(&payload, 9);
    let hash = Sha256::digest(&payload).into();
    let mut trailing = stored.clone();
    trailing.push(0);
    let mut damaged = stored.clone();
    let last = damaged.len() - 1;
    damaged[last] ^= 1;
    let mut cases = vec![
        wire(1, &stored[..stored.len() - 1], payload.len(), hash),
        wire(1, &trailing, payload.len(), hash),
        wire(1, &damaged, payload.len(), hash),
        wire(1, &stored, payload.len() - 1, hash),
        wire(1, &stored, payload.len() + 1, hash),
        wire(1, &stored, 0, hash),
        wire(1, &stored, payload.len(), [0; 32]),
        wire(0, &payload, payload.len(), [0; 32]),
    ];
    let valid = wire(1, &stored, payload.len(), hash);
    for stop in [0, 24, 56, 127, valid.len() - 1] {
        cases.push(valid[..stop].to_vec());
    }
    for bytes in cases {
        let allocations = allocation_counter::measure(|| {
            assert!(super::super::decode(&FORMAT, &bytes, DictionaryLimits::default()).is_err());
        });
        assert_eq!(allocations.bytes_current, 0);
    }
    let allocations = allocation_counter::measure(|| {
        let limits = DictionaryLimits {
            max_decoded_bytes: payload.len() - 1,
            ..DictionaryLimits::default()
        };
        assert!(matches!(
            FrameInput::new(&FORMAT, &valid, limits),
            Err(DictionaryError::Limit {
                resource: "decoded bytes",
                ..
            })
        ));
    });
    assert_eq!(allocations.count_total, 0);
}

#[test]
fn abandoned_dictionary_frames_release_all_partial_sections_and_inflate_state() {
    let payload = vec![42; 200_000];
    for codec in [0, 1] {
        let stored = if codec == 0 {
            payload.clone()
        } else {
            miniz_oxide::deflate::compress_to_vec_zlib(&payload, 9)
        };
        let wire = wire(
            codec,
            &stored,
            payload.len(),
            Sha256::digest(&payload).into(),
        );
        for stop in [0, 1, 2, 7, 13, 31, 48] {
            let measured = allocation_counter::measure(|| {
                let mut input =
                    FrameInput::new(&FORMAT, &wire, DictionaryLimits::default()).unwrap();
                for _ in 0..stop {
                    let ControlFlow::Continue(next) = input.advance().unwrap() else {
                        panic!("fixture completed too early");
                    };
                    input = next;
                }
                drop(input);
            });
            assert_eq!(measured.bytes_current, 0, "codec {codec}, stop {stop}");
        }
    }
}

#[test]
fn dictionary_frame_failure_retires_completed_and_partial_sections_together() {
    let format = Format {
        section_count: 3,
        ..FORMAT
    };
    let sections = [
        Section {
            kind: 1,
            records: 3,
            bytes: b"first section".to_vec(),
        },
        Section {
            kind: 2,
            records: 11,
            bytes: vec![42; 100_000],
        },
        Section {
            kind: 3,
            records: 29,
            bytes: vec![73; 50_000],
        },
    ];
    let wire = super::super::encode(&format, &sections, DictionaryLimits::default()).unwrap();
    for stop in [1, 2, 7, 13, 25, 26, 27, 33, 37] {
        let measured = allocation_counter::measure(|| {
            let mut input = FrameInput::new(&format, &wire, DictionaryLimits::default()).unwrap();
            for _ in 0..stop {
                let ControlFlow::Continue(next) = input.advance().unwrap() else {
                    panic!("fixture completed too early");
                };
                input = next;
            }
            assert!(!input.sections.is_empty());
            if stop >= 26 {
                assert_eq!(input.sections.len(), 2);
            }
            drop(input);
        });
        assert_eq!(measured.bytes_current, 0, "stop {stop}");
    }
    let mut damaged = wire;
    let last = damaged.len() - 1;
    damaged[last] ^= 1;
    let mut completed = 0;
    let measured = allocation_counter::measure(|| {
        let mut input = FrameInput::new(&format, &damaged, DictionaryLimits::default()).unwrap();
        loop {
            completed = completed.max(input.sections.len());
            match input.advance() {
                Ok(ControlFlow::Continue(next)) => input = next,
                Ok(ControlFlow::Break(_)) => panic!("corrupt final section was published"),
                Err(_) => break,
            }
        }
    });
    assert_eq!(completed, 2);
    assert_eq!(measured.bytes_current, 0);
}
