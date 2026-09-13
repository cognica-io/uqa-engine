//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Minimized suffix sharing must preserve exact UTF-16 lexical ranks and valid paths.

use super::{Builder, Lexicon};
use crate::nori::io::{Reader, Writer};

fn build(words: &[&str]) -> Lexicon {
    let mut builder = Builder::new();
    for word in words {
        builder.insert(word.encode_utf16().collect()).unwrap();
    }
    builder.finish(32).unwrap()
}

#[test]
fn shared_accepting_suffixes_keep_distinct_ranks_and_prefix_acceptance() {
    let words = ["a", "ab", "ac", "x", "xb", "xc"];
    let lexicon = build(&words);
    assert_eq!(lexicon.nodes.len(), 3);
    assert_eq!(lexicon.arcs.len(), 4);
    let mut output = Writer::default();
    lexicon.encode(&mut output).unwrap();
    let mut input = Reader::new(&output.0, "test lexicon", false);
    let decoded = Lexicon::decode(&mut input, 2).unwrap();
    input.finish().unwrap();
    for (rank, word) in words.iter().enumerate() {
        assert_eq!(decoded.lookup(word.encode_utf16()), Some(rank as u32));
    }
    assert_eq!(decoded.lookup("".encode_utf16()), None);
    assert_eq!(decoded.lookup("b".encode_utf16()), None);
    let enumerated: Vec<_> = decoded
        .entries()
        .map(|v| String::from_utf16(&v.unwrap()).unwrap())
        .collect();
    assert_eq!(enumerated, words);
}

#[test]
fn empty_models_empty_surfaces_and_supplementary_utf16_order_are_distinct() {
    let empty = build(&[]);
    assert_eq!(empty.len(), 0);
    assert_eq!(empty.entries().count(), 0);
    assert_eq!(empty.lookup([]), None);
    let words = ["", "가", "𐀀", "😀", "\u{e000}"];
    let lexicon = build(&words);
    assert_eq!(lexicon.len(), words.len());
    let enumerated: Vec<_> = lexicon
        .entries()
        .map(|v| String::from_utf16(&v.unwrap()).unwrap())
        .collect();
    assert_eq!(enumerated, words);
    assert_eq!(lexicon.lookup([0xd83d]), None);
    assert_eq!(lexicon.lookup([0xde00]), None);
    assert_eq!(lexicon.lookup([0xd83d, 0xde00]), Some(3));
}

#[test]
fn duplicate_unsorted_unpaired_surrogate_and_overlong_inputs_fail() {
    for words in [[vec![1], vec![1]], [vec![2], vec![1]]] {
        let mut builder = Builder::new();
        builder.insert(words[0].clone()).unwrap();
        assert!(builder.insert(words[1].clone()).is_err());
    }
    for units in [
        vec![0xd800],
        vec![0xdc00],
        vec![0xd800, 65],
        vec![65, 0xdc00],
    ] {
        let mut builder = Builder::new();
        builder.insert(units).unwrap();
        assert!(builder.finish(32).is_err());
    }
    let mut builder = Builder::new();
    builder.insert(vec![65; 33]).unwrap();
    assert!(builder.finish(32).is_err());
}
