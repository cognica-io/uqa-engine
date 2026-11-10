//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Builder, State};
use crate::morphology::{
    io::Writer,
    lexicon::{tests::input::wire, Lexicon},
    work::Work,
    DictionaryError, DictionaryResult,
};
use uqa_core::{
    image_io::{ImageDecodeWindow, ImageDecoder, ImageIoError},
    memory::MemoryBudget,
    CancellationToken,
};

const WINDOW: ImageDecodeWindow = ImageDecodeWindow { bytes: 1, steps: 1 };

struct NoRead;
impl std::io::Read for NoRead {
    fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
        panic!("native lexical construction must not read another image")
    }
}

fn input(
    surfaces: Vec<Vec<u16>>,
    maximum: usize,
    memory: &MemoryBudget,
    token: &CancellationToken,
) -> ImageDecoder<Lexicon, DictionaryError> {
    ImageDecoder::new(memory, token, |mut channel| async move {
        let mut builder = Builder::new();
        let mut work = channel.input();
        for surface in surfaces {
            builder = builder.insert_with(surface, &mut work).await?;
        }
        builder.finish_with(maximum, &mut work).await
    })
    .unwrap()
}

fn surfaces(deep: bool) -> Vec<Vec<u16>> {
    if deep {
        vec![vec![u16::from(b'a'); 6000]]
    } else {
        (1..=5000).map(|unit| vec![unit]).collect()
    }
}

#[test]
fn native_lexicon_builder_retains_wide_arcs_deep_paths_and_exact_wire_records() {
    for deep in [false, true] {
        for steps in [1, 13, 512] {
            let memory = MemoryBudget::new(1 << 20);
            let token = CancellationToken::new();
            let mut input = input(surfaces(deep), 6000, &memory, &token);
            let mut calls = 0;
            let lexicon = loop {
                calls += 1;
                assert!(calls < 100_000);
                if let Some(lexicon) = input
                    .advance(&mut NoRead, ImageDecodeWindow { bytes: 1, steps })
                    .unwrap()
                {
                    break lexicon;
                }
            };
            if steps == 1 {
                assert!(calls > 20_000);
            }
            let mut output = Writer::default();
            lexicon.encode(&mut output).unwrap();
            assert_eq!(output.0, wire(deep));
            assert_eq!(lexicon.len(), if deep { 1 } else { 5000 });
            for (rank, surface) in surfaces(deep).into_iter().enumerate() {
                assert_eq!(lexicon.lookup(surface), Some(rank as u32));
            }
            assert_eq!(memory.used(), 0);
            assert!(input.advance(&mut NoRead, WINDOW).is_err());
        }
    }
}

#[test]
fn native_lexicon_builder_failure_retires_input_pending_paths_and_interned_states() {
    for deep in [false, true] {
        for stop in [0, 1, 2, 128, 512, 4096, 12_000] {
            for mode in ["drop", "cancel", "window"] {
                let measured = allocation_counter::measure(|| {
                    let memory = MemoryBudget::new(1 << 20);
                    let token = CancellationToken::new();
                    let mut input = input(surfaces(deep), 6000, &memory, &token);
                    for _ in 0..stop {
                        assert!(input.advance(&mut NoRead, WINDOW).unwrap().is_none());
                    }
                    match mode {
                        "cancel" => {
                            token.cancel();
                            assert!(matches!(
                                input.advance(&mut NoRead, WINDOW),
                                Err(DictionaryError::Input(ImageIoError::Cancelled(_)))
                            ));
                        }
                        "window" => {
                            assert!(matches!(
                                input
                                    .advance(&mut NoRead, ImageDecodeWindow { bytes: 1, steps: 0 }),
                                Err(DictionaryError::Input(ImageIoError::Invalid))
                            ));
                        }
                        _ => (),
                    }
                    if mode != "drop" {
                        assert!(input.advance(&mut NoRead, WINDOW).is_err());
                    }
                    drop(input);
                    assert_eq!(memory.used(), 0);
                });
                assert_eq!(
                    measured.bytes_current, 0,
                    "deep {deep}, stop {stop}, {mode}"
                );
            }
        }
    }
    for (surfaces, maximum, message) in [
        (
            vec![vec![97; 6000], vec![97; 6000]],
            6000,
            "duplicate or not in UTF-16 order",
        ),
        (
            vec![vec![98; 6000], vec![97; 6000]],
            6000,
            "duplicate or not in UTF-16 order",
        ),
        (
            vec![vec![0xd800]],
            6000,
            "surface ends inside a surrogate pair",
        ),
        (vec![vec![97; 6000]], 1, "exceeding limit 1"),
    ] {
        let measured = allocation_counter::measure(|| {
            let memory = MemoryBudget::new(1 << 20);
            let token = CancellationToken::new();
            let mut input = input(surfaces.clone(), maximum, &memory, &token);
            let error = loop {
                match input.advance(&mut NoRead, WINDOW) {
                    Ok(None) => (),
                    Ok(Some(_)) => panic!("invalid lexical candidate was published"),
                    Err(error) => break error,
                }
            };
            assert!(error.to_string().contains(message), "{error}");
            assert!(input.advance(&mut NoRead, WINDOW).is_err());
            drop(input);
            assert_eq!(memory.used(), 0);
        });
        assert_eq!(measured.bytes_current, 0);
    }
}

async fn collision_graph(work: &mut impl Work) -> DictionaryResult<Lexicon> {
    let mut builder = Builder::new();
    let leaf = builder
        .intern_hashed(
            State {
                terminal: true,
                arcs: vec![],
            },
            0,
            work,
        )
        .await?;
    assert_eq!(leaf, 0);
    for (terminal, label, expected) in [
        (false, 97, 1),
        (false, 98, 2),
        (true, 97, 3),
        (false, 97, 1),
    ] {
        let state = State {
            terminal,
            arcs: vec![(label, leaf)],
        };
        assert_eq!(builder.intern_hashed(state, 0, work).await?, expected);
    }
    for (last_target, expected) in [(leaf, 4), (1, 5), (leaf, 4)] {
        let mut arcs = Vec::new();
        for label in 1..=5000 {
            work.records(label as usize - 1).await?;
            arcs.push((label, if label == 5000 { last_target } else { leaf }));
        }
        let state = State {
            terminal: false,
            arcs,
        };
        assert_eq!(builder.intern_hashed(state, 0, work).await?, expected);
    }
    let root = builder
        .intern_hashed(
            State {
                terminal: false,
                arcs: (1..=5)
                    .map(|target| (u16::from(b'r') + target as u16 - 1, target))
                    .collect(),
            },
            0,
            work,
        )
        .await?;
    let lexicon = Lexicon {
        nodes: builder.nodes,
        arcs: builder.arcs,
        root,
    };
    lexicon.validate_with(3, work).await?;
    Ok(lexicon)
}

#[test]
fn native_lexicon_builder_hash_collisions_compare_complete_signatures_before_reuse() {
    let measured = allocation_counter::measure(|| {
        let memory = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let mut input =
            ImageDecoder::<_, DictionaryError>::new(&memory, &token, |mut channel| async move {
                collision_graph(&mut channel.input()).await
            })
            .unwrap();
        let mut calls = 0;
        let lexicon = loop {
            calls += 1;
            assert!(calls < 50_000);
            if let Some(lexicon) = input.advance(&mut NoRead, WINDOW).unwrap() {
                break lexicon;
            }
        };
        assert!(calls > 10_000);
        assert_eq!(lexicon.nodes.len(), 7);
        assert_eq!(lexicon.len(), 10_004);
        for (word, rank) in [("ra", 0), ("sb", 1), ("t", 2), ("ta", 3)] {
            assert_eq!(lexicon.lookup(word.encode_utf16()), Some(rank));
        }
        for label in 1..=5000 {
            assert_eq!(
                lexicon.lookup([u16::from(b'u'), label]),
                Some(4 + u32::from(label) - 1)
            );
        }
        for label in 1..5000 {
            assert_eq!(
                lexicon.lookup([u16::from(b'v'), label]),
                Some(5004 + u32::from(label) - 1)
            );
        }
        assert_eq!(lexicon.lookup([u16::from(b'v'), 5000]), None);
        assert_eq!(lexicon.lookup([u16::from(b'v'), 5000, 97]), Some(10_003));
        assert_eq!(memory.used(), 0);
    });
    assert_eq!(measured.bytes_current, 0);
}
