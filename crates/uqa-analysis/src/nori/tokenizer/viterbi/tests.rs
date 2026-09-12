//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Candidate ties, future-path pruning, and EOS costs are observable path-selection rules.

use super::{analyze, State};
use crate::nori::frame;
use crate::nori::tokenizer::lattice::{Lattice, Node, WordId};
use crate::nori::{
    DecompoundMode, DictionaryLimits, NoriDictionary, NoriLimits, NoriOptions, POSType,
};

#[test]
fn forced_backtrace_selects_the_first_cheapest_future_candidate_and_rebases_it() {
    let model = NoriDictionary::from_bytes(
        &crate::nori::tests::fixtures::bundle(),
        DictionaryLimits::default(),
    )
    .unwrap();
    let input = vec!['가' as u16; 1030];
    let limits = NoriLimits::default();
    let mut poll = || Ok(());
    let mut state = State {
        input: &input,
        model: &model,
        user: None,
        options: NoriOptions {
            decompound_mode: DecompoundMode::None,
            ..NoriOptions::default()
        },
        lattice: Lattice::new(limits).unwrap(),
        position: 1024,
        last_backtrace: 0,
        pending: Vec::new(),
        ngram: None,
        limits,
        total_tokens: 0,
        output_units: 0,
        work: 0,
        poll: &mut poll,
    };
    for (end, cost, word_pos) in [(1024, 50, 0), (1025, 3, 0), (1025, 3, 1), (1026, 3, 0)] {
        state
            .lattice
            .push(
                end,
                Node {
                    cost,
                    right: 2,
                    back_pos: 0,
                    word_pos,
                    back_index: 0,
                    word: WordId::Known(0),
                },
            )
            .unwrap();
    }
    assert!(!state.forward().unwrap());
    assert_eq!(state.position, 1025);
    assert_eq!(state.last_backtrace, 1025);
    assert_eq!(state.pending.len(), 1);
    assert_eq!(state.pending[0].start_utf16, 0);
    assert_eq!(state.pending[0].end_utf16, 1025);
    assert_eq!(state.lattice.get(1025).len(), 1);
    assert_eq!(state.lattice.get(1025)[0].cost, 0);
    assert_eq!(state.lattice.get(1025)[0].right, 2);
    assert!(state.lattice.get(1026).is_empty());
    assert_eq!(state.lattice.next_pos(), 1027);
}

#[test]
fn eos_connection_cost_can_select_the_more_expensive_partial_path() {
    let mut sections = crate::nori::tests::fixtures::sections();
    // Both homographs match 가. The inflection costs five more before EOS, but its right context saves 22.
    let word = 8 + 2 * 32;
    sections[1].bytes[word + 6..word + 8].copy_from_slice(&0_u16.to_le_bytes());
    sections[1].bytes[word + 8..word + 10].copy_from_slice(&(-118_i16).to_le_bytes());
    let bytes = frame::encode(&sections, DictionaryLimits::default()).unwrap();
    let model = NoriDictionary::from_bytes(&bytes, DictionaryLimits::default()).unwrap();
    let input = ['가' as u16];
    let options = NoriOptions {
        decompound_mode: DecompoundMode::None,
        ..NoriOptions::default()
    };
    let output = analyze(
        &input,
        &model,
        None,
        options,
        NoriLimits::default(),
        &mut || Ok(()),
    )
    .unwrap();
    assert_eq!(output.tokens.len(), 1);
    assert_eq!(output.tokens[0].pos_type, POSType::Inflect);
    assert_eq!(output.tokens[0].term_utf16, input);
    let output = analyze(
        &input,
        &model,
        None,
        NoriOptions::default(),
        NoriLimits::default(),
        &mut || Ok(()),
    )
    .unwrap();
    assert!(output.tokens.is_empty());
    assert_eq!(output.final_offset_utf16, 1);
    assert_eq!(output.final_position_increment, 0);
}
