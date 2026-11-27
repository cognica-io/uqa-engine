//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::rpq::{epsilon_closure, parse_rpq, NfaTransition};

#[test]
fn dense_path_states_match_direct_nfa_steps_and_the_public_view() {
    for expression in ["a", "(a|b)*/b", "(a/b){0,3}", "a*/(a|b)*"] {
        let path = parse_rpq(expression).unwrap();
        let nfa = build_nfa(&simplify(&path).unwrap()).unwrap();
        let public = subset_construction(&nfa).unwrap();
        let compiled = CompiledDfa::from_path(&path).unwrap();
        for length in 0..=5 {
            for mut word in 0..3_usize.pow(length) {
                let mut direct = epsilon_closure(&nfa, &BTreeSet::from([nfa.start])).unwrap();
                let mut subset = Some(&public.start);
                let mut id = Some(CompiledDfa::START);
                for _ in 0..length {
                    let label = ["a", "b", "unknown"][word % 3];
                    word /= 3;
                    let mut targets = BTreeSet::new();
                    for &state in &direct {
                        for edge in &nfa.transitions[state as usize] {
                            if edge.label.as_deref() == Some(label) {
                                targets.insert(edge.target);
                            }
                        }
                    }
                    direct = epsilon_closure(&nfa, &targets).unwrap();
                    subset = subset.and_then(|s| public.transitions[s].get(label));
                    id = id.and_then(|s| compiled.transitions(s).get(label).copied());
                    assert_eq!(subset, (!direct.is_empty()).then_some(&direct));
                    assert_eq!(id.is_some(), subset.is_some());
                }
                assert_eq!(
                    id.is_some_and(|s| compiled.accepting(s)),
                    direct.contains(&nfa.accept)
                );
            }
        }
    }
}

#[test]
fn looping_over_dense_path_states_requires_no_heap_allocation() {
    let compiled = CompiledDfa::from_path(&parse_rpq("(a|b)*").unwrap()).unwrap();
    let mut state = CompiledDfa::START;
    let allocated = allocation_counter::measure(|| {
        for i in 0..8192 {
            state = compiled.transitions(state)[if i % 2 == 0 { "a" } else { "b" }];
        }
    });
    assert!(compiled.accepting(state));
    assert_eq!(allocated.count_total, 0);
}

#[test]
fn public_closure_validates_the_automaton_before_external_seed_ids() {
    let mut nfa = build_nfa(&RegularPathExpr::label("a")).unwrap();
    let bad = BTreeSet::from([8, 9]);
    assert!(
        matches!(epsilon_closure(&nfa, &bad), Err(RPQBuildError::InvalidNfa(e)) if e == "closure state 9 is outside the NFA")
    );
    nfa.transitions[0].push(NfaTransition {
        label: None,
        target: 10,
    });
    assert!(
        matches!(epsilon_closure(&nfa, &bad), Err(RPQBuildError::InvalidNfa(e)) if e.contains("targets missing state 10"))
    );
}
