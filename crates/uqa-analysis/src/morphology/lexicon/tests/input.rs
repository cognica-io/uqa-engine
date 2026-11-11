//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Lexicon, Reader, Writer};
use crate::morphology::DictionaryError;
use std::sync::Arc;
use uqa_core::{
    image_io::{ImageDecodeWindow, ImageDecoder, ImageIoError},
    memory::MemoryBudget,
    CancellationToken,
};

const WINDOW: ImageDecodeWindow = ImageDecodeWindow { bytes: 1, steps: 1 };

struct NoRead;
impl std::io::Read for NoRead {
    fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
        panic!("borrowed lexical tables must not reopen their source")
    }
}

/// Independent wire graphs: one shared accepting suffix with 5,000 arcs, or one 6,000-unit path.
pub(in crate::morphology::lexicon) fn wire(deep: bool) -> Vec<u8> {
    let (root, nodes, arcs) = if deep {
        (6000_u32, 6001_u32, 6000_u32)
    } else {
        (1, 2, 5000)
    };
    let mut bytes = Vec::new();
    for value in [root, nodes, arcs] {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    for node in 0..nodes {
        let count = if node == 0 {
            0
        } else if deep {
            1
        } else {
            arcs
        };
        bytes.extend_from_slice(&count.to_le_bytes());
        bytes.push(u8::from(node == 0));
    }
    for arc in 0..arcs {
        let label = if deep {
            u16::from(b'a')
        } else {
            (arc + 1) as u16
        };
        bytes.extend_from_slice(&label.to_le_bytes());
        bytes.extend_from_slice(&(if deep { arc } else { 0 }).to_le_bytes());
    }
    bytes
}

fn input(
    bytes: Arc<Vec<u8>>,
    memory: &MemoryBudget,
    token: &CancellationToken,
) -> ImageDecoder<Lexicon, DictionaryError> {
    ImageDecoder::new(memory, token, |mut channel| async move {
        let mut reader = Reader::new(&bytes, "test lexical input", false);
        let lexicon = Lexicon::decode_with(&mut reader, 6000, &mut channel.input()).await?;
        reader.finish()?;
        Ok(lexicon)
    })
    .unwrap()
}

#[test]
fn lexical_input_retains_wide_arcs_deep_paths_and_shared_suffixes_across_polls() {
    for deep in [false, true] {
        let bytes = Arc::new(wire(deep));
        let memory = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let mut input = input(bytes.clone(), &memory, &token);
        let mut polls = 0;
        let lexicon = loop {
            polls += 1;
            assert!(polls < 50_000);
            if let Some(value) = input.advance(&mut NoRead, WINDOW).unwrap() {
                break value;
            }
        };
        assert!(polls > 5000);
        assert_eq!(lexicon.nodes.capacity(), if deep { 6001 } else { 2 });
        assert_eq!(lexicon.arcs.capacity(), if deep { 6000 } else { 5000 });
        assert_eq!(lexicon.len(), if deep { 1 } else { 5000 });
        if deep {
            assert_eq!(
                lexicon.lookup(std::iter::repeat_n(u16::from(b'a'), 6000)),
                Some(0)
            );
            assert_eq!(
                lexicon.lookup(std::iter::repeat_n(u16::from(b'a'), 5999)),
                None
            );
        } else {
            for label in 1..=5000 {
                assert_eq!(lexicon.lookup([label]), Some(u32::from(label - 1)));
            }
        }
        let mut output = Writer::default();
        lexicon.encode(&mut output).unwrap();
        assert_eq!(output.0, *bytes);
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn lexical_input_rejection_and_abandonment_retire_every_native_table_and_validation_stack() {
    for deep in [false, true] {
        let bytes = Arc::new(wire(deep));
        for stop in [0, 1, 40, 180, 500, 4500] {
            for cancel in [false, true] {
                let measured = allocation_counter::measure(|| {
                    let memory = MemoryBudget::new(1 << 20);
                    let token = CancellationToken::new();
                    let mut input = input(bytes.clone(), &memory, &token);
                    for _ in 0..stop {
                        assert!(input.advance(&mut NoRead, WINDOW).unwrap().is_none());
                    }
                    if cancel {
                        token.cancel();
                        assert!(matches!(
                            input.advance(&mut NoRead, WINDOW),
                            Err(DictionaryError::Input(ImageIoError::Cancelled(_)))
                        ));
                        assert!(input.advance(&mut NoRead, WINDOW).is_err());
                    }
                    drop(input);
                    assert_eq!(memory.used(), 0);
                });
                assert_eq!(measured.bytes_current, 0, "deep {deep}, stop {stop}");
            }
        }
    }
    for kind in ["order", "target", "surrogate", "unreachable"] {
        let deep = kind == "unreachable";
        let mut bytes = wire(deep);
        let start = 12 + if deep { 6001 * 5 } else { 2 * 5 };
        match kind {
            "order" => bytes[start + 6..start + 8].copy_from_slice(&1_u16.to_le_bytes()),
            "target" => bytes[start + 2..start + 6].copy_from_slice(&1_u32.to_le_bytes()),
            "surrogate" => bytes[start + 4999 * 6..start + 4999 * 6 + 2]
                .copy_from_slice(&0xdc00_u16.to_le_bytes()),
            _ => bytes[start + 5999 * 6 + 2..start + 6000 * 6]
                .copy_from_slice(&5998_u32.to_le_bytes()),
        }
        let expected = Lexicon::decode(&mut Reader::new(&bytes, "test lexical input", false), 6000)
            .unwrap_err()
            .to_string();
        let diagnostic = match kind {
            "order" => "unordered",
            "target" => "not an earlier state",
            "surrogate" => "unpaired surrogate",
            _ => "unreachable states",
        };
        assert!(expected.contains(diagnostic), "{kind}: {expected}");
        let bytes = Arc::new(bytes);
        let memory = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let measured = allocation_counter::measure(|| {
            let mut input = input(bytes.clone(), &memory, &token);
            let error = loop {
                match input.advance(&mut NoRead, WINDOW) {
                    Ok(None) => (),
                    Err(error) => break error,
                    Ok(Some(_)) => panic!("invalid lexical graph published: {kind}"),
                }
            };
            assert_eq!(error.to_string(), expected);
            assert!(input.advance(&mut NoRead, WINDOW).is_err());
        });
        assert_eq!(measured.bytes_current, 0, "{kind}");
        assert_eq!(memory.used(), 0);
    }
}
